/*
 * H.264 passthrough for PJSUA video.
 *
 * The application supplies already-encoded H.264 access units and receives
 * the remote's, while PJSUA keeps doing SDP, RTP/RTCP, SRTP and ICE.
 *
 * Three pieces, all registered by pjrs_vpt_init():
 *
 *  - A private video format, AUH4: a fixed-size buffer holding one access
 *    unit behind a small header. PJSUA's video bridge and ports only move
 *    frames of exactly the format's framebytes.
 *
 *  - An "H264" codec whose decoded format is AUH4. Encoding packetizes the
 *    access unit (RFC 6184 non-interleaved); decoding reassembles it.
 *
 *  - A passive capture device and a passive render device, both AUH4.
 *    Received frames go to the application from the decoder, not the
 *    render device: that is clocked, and PJSUA overwrites a decoded frame
 *    the clock has not collected yet when packets arrive in a burst.
 *
 * Capture runs on two clocks (video port polling the device, bridge feeding
 * the encoder), so polling never removes a frame. The device returns the
 * oldest unsent one tagged with its id; the codec reports it consumed once
 * encoded, and skips empty frames and ids it has already handled.
 */

#include <pjsua-lib/pjsua.h>
#include <pjsua-lib/pjsua_internal.h>
#include <pjmedia.h>
#include <pjmedia-codec.h>
#include <pjmedia-videodev/videodev_imp.h>
#include <pjmedia/vid_codec_util.h>
#include <pjmedia-codec/h264_packetizer.h>
#include <pj/assert.h>
#include <pj/log.h>
#include <pj/string.h>

#if defined(PJMEDIA_HAS_VIDEO) && PJMEDIA_HAS_VIDEO != 0

#include "video_passthrough.h"

#define THIS_FILE           "video_passthrough.c"
#define DRIVER_NAME         "Passthrough"
#define CAPTURE_DEV_NAME    "Passthrough capture"
#define RENDER_DEV_NAME     "Passthrough render"
#define CLOCK_RATE          90000

#define PJRS_FORMAT_AUH4    PJMEDIA_FORMAT_PACK('A', 'U', 'H', '4')
#define AU_MAGIC            0x34485541u /* "AUH4" little-endian */

/* Header in front of every AUH4 frame. */
typedef struct au_hdr
{
    pj_uint32_t magic;
    pj_uint32_t seq;
    pj_uint32_t len;
    pj_uint32_t flags;
} au_hdr;

#define AU_HDR_SIZE         ((unsigned)sizeof(au_hdr))
#define AU_FLAG_KEYFRAME    1u

static struct vpt_state
{
    pj_bool_t               inited;
    pj_pool_t              *pool;
    pj_pool_factory        *pf;
    /* Guards cfg and profile_level_id, which pjrs_vpt_reconfigure() can
     * change while PJSIP reads them for a new call. */
    pj_mutex_t             *lock;
    pjrs_vpt_config         cfg;
    pjrs_vpt_callbacks      cb;
    unsigned                framebytes;
    pjmedia_video_format_info vfi;
    pjmedia_vid_codec_factory codec_factory;
    pjmedia_vid_dev_factory   *dev_factory;
    char                    profile_level_id[8];
    /* Receive limits as fmtp values, when configured. */
    char                    max_fs[12];
    char                    max_mbps[12];
    char                    max_br[12];
    /* Bumped by pjrs_vpt_source_restarted(). keyed_epoch == source_epoch
     * once a keyframe of the current source stream has gone out, so a codec
     * reopened by a re-INVITE need not wait for the next one. Under lock. */
    pj_uint32_t             source_epoch;
    pj_uint32_t             keyed_epoch;
} vpt;

/* ------------------------------------------------------------------------
 * AUH4 format
 */

static pj_status_t auh4_apply_fmt(const pjmedia_video_format_info *vfi,
                                  pjmedia_video_apply_fmt_param *aparam)
{
    PJ_UNUSED_ARG(vfi);

    aparam->framebytes = vpt.framebytes;
    aparam->strides[0] = (int)vpt.framebytes;
    aparam->plane_bytes[0] = vpt.framebytes;
    aparam->planes[0] = aparam->buffer;
    return PJ_SUCCESS;
}

static unsigned is_keyframe(const pj_uint8_t *bits, unsigned len)
{
    unsigned i;

    /* An IDR slice or an SPS marks a decoder refresh point. */
    for (i = 0; i + 3 < len; ++i) {
        if (bits[i] == 0 && bits[i+1] == 0 && bits[i+2] == 1) {
            unsigned type = bits[i+3] & 0x1F;
            if (type == 5 || type == 7)
                return 1;
            i += 2;
        }
    }
    return 0;
}

/* Size of the largest NAL unit in an Annex-B access unit, start codes
 * excluded.
 */
static unsigned largest_nal(const pj_uint8_t *bits, unsigned len)
{
    unsigned i, start = 0, largest = 0;
    pj_bool_t in_nal = PJ_FALSE;

    for (i = 0; i + 2 < len; ++i) {
        if (bits[i] == 0 && bits[i+1] == 0 && bits[i+2] == 1) {
            if (in_nal) {
                unsigned end = i;
                /* A four-byte start code's leading zero is not payload. */
                while (end > start && bits[end-1] == 0)
                    --end;
                if (end - start > largest)
                    largest = end - start;
            }
            start = i + 3;
            in_nal = PJ_TRUE;
            i += 2;
        }
    }
    if (in_nal && len - start > largest)
        largest = len - start;
    return largest;
}

/* ------------------------------------------------------------------------
 * Codec
 */

typedef struct vpt_codec
{
    pj_pool_t                  *pool;
    pjmedia_vid_codec_param    *prm;
    pjmedia_h264_packetizer    *pktz;

    /* Encoder */
    pj_uint8_t                 *enc_buf;
    unsigned                    enc_len;
    unsigned                    enc_pos;
    pj_bool_t                   enc_key;
    pj_uint32_t                 enc_last_seq;
    pj_bool_t                   enc_have_seq;
    pj_bool_t                   enc_sent_key;
    pj_uint32_t                 enc_epoch;
    pj_bool_t                   enc_single_nal;
    unsigned                    oversize_drops;

    /* Decoder */
    pj_uint8_t                 *dec_buf;
    unsigned                    dec_buf_size;
    pj_uint32_t                 dec_seq;
} vpt_codec;

/* A consistent copy of the configuration. */
static pjrs_vpt_config cur_cfg(void)
{
    pjrs_vpt_config cfg;

    pj_mutex_lock(vpt.lock);
    cfg = vpt.cfg;
    pj_mutex_unlock(vpt.lock);
    return cfg;
}

static void request_keyframe(void)
{
    if (vpt.cb.keyframe_request)
        (*vpt.cb.keyframe_request)(vpt.cb.user_data);
}

static pj_status_t vc_test_alloc(pjmedia_vid_codec_factory *f,
                                 const pjmedia_vid_codec_info *info)
{
    PJ_ASSERT_RETURN(f == &vpt.codec_factory, PJ_EINVAL);
    if (info->fmt_id == PJMEDIA_FORMAT_H264 && info->pt == vpt.cfg.pt)
        return PJ_SUCCESS;
    return PJMEDIA_CODEC_EUNSUP;
}

static pj_status_t vc_default_attr(pjmedia_vid_codec_factory *f,
                                   const pjmedia_vid_codec_info *info,
                                   pjmedia_vid_codec_param *attr)
{
    pjrs_vpt_config cfg;

    PJ_ASSERT_RETURN(f == &vpt.codec_factory, PJ_EINVAL);
    PJ_ASSERT_RETURN(info && attr, PJ_EINVAL);
    cfg = cur_cfg();

    pj_bzero(attr, sizeof(*attr));
    attr->dir = PJMEDIA_DIR_ENCODING_DECODING;
    attr->packing = PJMEDIA_VID_PACKING_PACKETS;

    /* poll_fps, not fps: see pjrs_vpt_config. */
    pjmedia_format_init_video(&attr->enc_fmt, PJMEDIA_FORMAT_H264,
                              cfg.width, cfg.height,
                              cfg.poll_fps, 1);
    pjmedia_format_init_video(&attr->dec_fmt, PJRS_FORMAT_AUH4,
                              cfg.width, cfg.height,
                              cfg.poll_fps, 1);

    attr->dec_fmtp.cnt = 2;
    attr->dec_fmtp.param[0].name = pj_str((char*)"profile-level-id");
    attr->dec_fmtp.param[0].val = pj_str(vpt.profile_level_id);
    attr->dec_fmtp.param[1].name = pj_str((char*)"packetization-mode");
    attr->dec_fmtp.param[1].val = pj_str(cfg.packetization_mode ?
                                         (char*)"1" : (char*)"0");
    if (cfg.max_fs) {
        attr->dec_fmtp.param[attr->dec_fmtp.cnt].name = pj_str((char*)"max-fs");
        attr->dec_fmtp.param[attr->dec_fmtp.cnt++].val = pj_str(vpt.max_fs);
    }
    if (cfg.max_mbps) {
        attr->dec_fmtp.param[attr->dec_fmtp.cnt].name = pj_str((char*)"max-mbps");
        attr->dec_fmtp.param[attr->dec_fmtp.cnt++].val = pj_str(vpt.max_mbps);
    }
    if (cfg.max_br) {
        attr->dec_fmtp.param[attr->dec_fmtp.cnt].name = pj_str((char*)"max-br");
        attr->dec_fmtp.param[attr->dec_fmtp.cnt++].val = pj_str(vpt.max_br);
    }

    attr->enc_fmt.det.vid.avg_bps = cfg.avg_bps;
    attr->enc_fmt.det.vid.max_bps = cfg.max_bps;
    attr->enc_mtu = PJMEDIA_MAX_VID_PAYLOAD_SIZE;

    /* The source's resolution is fixed by whoever encodes it; do not let
     * the remote's fmtp rescale enc_fmt into something it isn't.
     */
    attr->ignore_fmtp = PJ_TRUE;

    return PJ_SUCCESS;
}

static pj_status_t vc_enum_info(pjmedia_vid_codec_factory *f,
                                unsigned *count,
                                pjmedia_vid_codec_info info[])
{
    pjrs_vpt_config cfg;

    PJ_ASSERT_RETURN(info && *count > 0, PJ_EINVAL);
    PJ_ASSERT_RETURN(f == &vpt.codec_factory, PJ_EINVAL);
    cfg = cur_cfg();

    *count = 1;
    pj_bzero(&info[0], sizeof(info[0]));
    info[0].fmt_id = PJMEDIA_FORMAT_H264;
    info[0].pt = cfg.pt;
    info[0].encoding_name = pj_str((char*)"H264");
    info[0].encoding_desc = pj_str((char*)"H.264 passthrough");
    info[0].clock_rate = CLOCK_RATE;
    info[0].dir = PJMEDIA_DIR_ENCODING_DECODING;
    info[0].dec_fmt_id_cnt = 1;
    info[0].dec_fmt_id[0] = PJRS_FORMAT_AUH4;
    info[0].packings = PJMEDIA_VID_PACKING_PACKETS;
    info[0].fps_cnt = 1;
    info[0].fps[0].num = cfg.poll_fps;
    info[0].fps[0].denum = 1;
    return PJ_SUCCESS;
}

static pjmedia_vid_codec_op vc_op;

static pj_status_t vc_alloc_codec(pjmedia_vid_codec_factory *f,
                                  const pjmedia_vid_codec_info *info,
                                  pjmedia_vid_codec **p_codec)
{
    pj_pool_t *pool;
    pjmedia_vid_codec *codec;
    vpt_codec *vc;

    PJ_ASSERT_RETURN(f == &vpt.codec_factory && info && p_codec, PJ_EINVAL);

    pool = pj_pool_create(vpt.pf, "vptc%p", 512, 512, NULL);
    if (!pool)
        return PJ_ENOMEM;

    codec = PJ_POOL_ZALLOC_T(pool, pjmedia_vid_codec);
    codec->factory = f;
    codec->op = &vc_op;

    vc = PJ_POOL_ZALLOC_T(pool, vpt_codec);
    vc->pool = pool;
    codec->codec_data = vc;

    *p_codec = codec;
    return PJ_SUCCESS;
}

static pj_status_t vc_dealloc_codec(pjmedia_vid_codec_factory *f,
                                    pjmedia_vid_codec *codec)
{
    vpt_codec *vc;

    PJ_UNUSED_ARG(f);
    PJ_ASSERT_RETURN(codec, PJ_EINVAL);

    vc = (vpt_codec*)codec->codec_data;
    pj_pool_release(vc->pool);
    return PJ_SUCCESS;
}

static pj_status_t vc_init(pjmedia_vid_codec *codec, pj_pool_t *pool)
{
    PJ_UNUSED_ARG(codec);
    PJ_UNUSED_ARG(pool);
    return PJ_SUCCESS;
}

static void notify_open(const pjmedia_vid_codec_param *prm)
{
    pjrs_vpt_stream_info si;

    if (!vpt.cb.encoder_open || !(prm->dir & PJMEDIA_DIR_ENCODING))
        return;

    pj_bzero(&si, sizeof(si));
    si.width = prm->enc_fmt.det.vid.size.w;
    si.height = prm->enc_fmt.det.vid.size.h;
    si.fps_num = cur_cfg().fps;
    si.fps_denum = 1;
    si.avg_bps = prm->enc_fmt.det.vid.avg_bps;
    si.max_bps = prm->enc_fmt.det.vid.max_bps;
    (*vpt.cb.encoder_open)(vpt.cb.user_data, &si);
}

static pj_status_t vc_open(pjmedia_vid_codec *codec,
                           pjmedia_vid_codec_param *param)
{
    vpt_codec *vc;
    pjmedia_vid_codec_h264_fmtp fmtp;
    pjmedia_h264_packetizer_cfg pcfg;
    pj_status_t status;

    PJ_ASSERT_RETURN(codec && param, PJ_EINVAL);

    vc = (vpt_codec*)codec->codec_data;
    vc->prm = pjmedia_vid_codec_param_clone(vc->pool, param);

    pj_bzero(&fmtp, sizeof(fmtp));
    status = pjmedia_vid_codec_h264_parse_fmtp(&vc->prm->enc_fmtp, &fmtp);
    if (status != PJ_SUCCESS)
        return status;

    /* Mode 0 is what a peer naming no mode means. Access units with a NAL
     * unit over the MTU are then dropped (see drop_oversized).
     */
    if (fmtp.packetization_mode != PJMEDIA_H264_PACKETIZER_MODE_NON_INTERLEAVED &&
        fmtp.packetization_mode != PJMEDIA_H264_PACKETIZER_MODE_SINGLE_NAL)
    {
        PJ_LOG(3,(THIS_FILE, "Remote packetization-mode %d unsupported",
                  fmtp.packetization_mode));
        return PJ_ENOTSUP;
    }

    pj_bzero(&pcfg, sizeof(pcfg));
    pcfg.mtu = vc->prm->enc_mtu;
    pcfg.mode = (pjmedia_h264_packetizer_mode)fmtp.packetization_mode;
    PJ_LOG(4,(THIS_FILE, "Packetization mode %d, MTU %d", pcfg.mode, pcfg.mtu));
    pcfg.unpack_nal_start = 4;
    status = pjmedia_h264_packetizer_create(vc->pool, &pcfg, &vc->pktz);
    if (status != PJ_SUCCESS)
        return status;

    vc->enc_buf = (pj_uint8_t*)pj_pool_alloc(vc->pool, vpt.cfg.max_frame_size);
    vc->dec_buf_size = vpt.cfg.max_frame_size;
    vc->dec_buf = (pj_uint8_t*)pj_pool_alloc(vc->pool, vc->dec_buf_size);
    vc->enc_len = vc->enc_pos = 0;
    vc->enc_have_seq = PJ_FALSE;
    vc->enc_sent_key = PJ_FALSE;
    vc->enc_epoch = 0;
    vc->enc_single_nal =
        (pcfg.mode == PJMEDIA_H264_PACKETIZER_MODE_SINGLE_NAL);
    vc->oversize_drops = 0;
    vc->dec_seq = 0;

    pj_memcpy(param, vc->prm, sizeof(*param));

    notify_open(vc->prm);

    /* Whatever the source has queued was encoded for nobody; the remote
     * cannot decode anything until it sees a keyframe.
     */
    if (vc->prm->dir & PJMEDIA_DIR_ENCODING)
        request_keyframe();

    return PJ_SUCCESS;
}

static pj_status_t vc_close(pjmedia_vid_codec *codec)
{
    vpt_codec *vc;

    PJ_ASSERT_RETURN(codec, PJ_EINVAL);
    vc = (vpt_codec*)codec->codec_data;
    if (vpt.cb.encoder_close && vc->prm &&
        (vc->prm->dir & PJMEDIA_DIR_ENCODING))
    {
        (*vpt.cb.encoder_close)(vpt.cb.user_data);
    }
    return PJ_SUCCESS;
}

static pj_status_t vc_modify(pjmedia_vid_codec *codec,
                             const pjmedia_vid_codec_param *param)
{
    vpt_codec *vc;

    PJ_ASSERT_RETURN(codec && param, PJ_EINVAL);
    vc = (vpt_codec*)codec->codec_data;

    vc->prm->enc_fmt.det.vid.avg_bps = param->enc_fmt.det.vid.avg_bps;
    vc->prm->enc_fmt.det.vid.max_bps = param->enc_fmt.det.vid.max_bps;
    notify_open(vc->prm);
    return PJ_SUCCESS;
}

static pj_status_t vc_get_param(pjmedia_vid_codec *codec,
                                pjmedia_vid_codec_param *param)
{
    vpt_codec *vc;

    PJ_ASSERT_RETURN(codec && param, PJ_EINVAL);
    vc = (vpt_codec*)codec->codec_data;
    pj_memcpy(param, vc->prm, sizeof(*param));
    return PJ_SUCCESS;
}

static void no_output(pjmedia_frame *output, const pjmedia_frame *input,
                      pj_bool_t *has_more)
{
    output->type = PJMEDIA_FRAME_TYPE_NONE;
    output->size = 0;
    output->timestamp = input->timestamp;
    *has_more = PJ_FALSE;
}

static void set_keyed(pj_uint32_t epoch)
{
    pj_mutex_lock(vpt.lock);
    vpt.keyed_epoch = epoch;
    pj_mutex_unlock(vpt.lock);
}

/* In single-NAL mode a NAL unit over the MTU cannot be sent, and everything
 * up to the next keyframe depends on it: drop the access unit and ask for a
 * keyframe. Reported as nothing sent, since the stream would only log an
 * error.
 */
static void drop_oversized(vpt_codec *vc, pjmedia_frame *output,
                           pj_bool_t *has_more)
{
    vc->enc_len = vc->enc_pos = 0;
    vc->enc_sent_key = PJ_FALSE;
    /* What follows references what was dropped. */
    set_keyed(0);
    *has_more = PJ_FALSE;
    output->size = 0;
    output->type = PJMEDIA_FRAME_TYPE_NONE;
    if (vc->oversize_drops++ % 100 == 0) {
        PJ_LOG(3,(THIS_FILE, "Access unit dropped, NAL unit too large for "
                  "packetization-mode 0 (%u so far)", vc->oversize_drops));
    }
    request_keyframe();
}

static pj_status_t vc_encode_more(pjmedia_vid_codec *codec,
                                  unsigned out_size,
                                  pjmedia_frame *output,
                                  pj_bool_t *has_more)
{
    vpt_codec *vc;
    const pj_uint8_t *payload;
    pj_size_t payload_len;
    pj_status_t status;

    PJ_ASSERT_RETURN(codec && out_size && output && has_more, PJ_EINVAL);
    vc = (vpt_codec*)codec->codec_data;

    if (vc->enc_pos >= vc->enc_len) {
        output->size = 0;
        output->type = PJMEDIA_FRAME_TYPE_NONE;
        *has_more = PJ_FALSE;
        return PJ_SUCCESS;
    }

    status = pjmedia_h264_packetize(vc->pktz, vc->enc_buf, vc->enc_len,
                                    &vc->enc_pos, &payload, &payload_len);
    if (status != PJ_SUCCESS) {
        /* Checked whole in encode_begin; this is only reached if the
         * packetizer's idea of a NAL unit differs from largest_nal's.
         */
        drop_oversized(vc, output, has_more);
        return PJ_SUCCESS;
    }
    PJ_ASSERT_RETURN(payload_len <= out_size, PJMEDIA_CODEC_EFRMTOOSHORT);

    output->type = PJMEDIA_FRAME_TYPE_VIDEO;
    pj_memcpy(output->buf, payload, payload_len);
    output->size = payload_len;
    if (vc->enc_key)
        output->bit_info |= PJMEDIA_VID_FRM_KEYFRAME;

    *has_more = (vc->enc_pos < vc->enc_len);
    return PJ_SUCCESS;
}

static pj_status_t vc_encode_begin(pjmedia_vid_codec *codec,
                                   const pjmedia_vid_encode_opt *opt,
                                   const pjmedia_frame *input,
                                   unsigned out_size,
                                   pjmedia_frame *output,
                                   pj_bool_t *has_more)
{
    vpt_codec *vc;
    const au_hdr *hdr;
    pj_uint32_t epoch;
    pj_bool_t keyed;

    PJ_ASSERT_RETURN(codec && input && out_size && output && has_more,
                     PJ_EINVAL);
    vc = (vpt_codec*)codec->codec_data;

    /* Keyframe requests (RTCP PLI/FIR, periodic keyframe config) go to the
     * source; the frame in hand is sent as is.
     */
    if (opt && opt->force_keyframe)
        request_keyframe();

    if (input->size < AU_HDR_SIZE) {
        no_output(output, input, has_more);
        return PJ_SUCCESS;
    }

    hdr = (const au_hdr*)input->buf;
    if (hdr->magic != AU_MAGIC || hdr->len == 0 ||
        hdr->len > input->size - AU_HDR_SIZE ||
        (vc->enc_have_seq && hdr->seq == vc->enc_last_seq))
    {
        no_output(output, input, has_more);
        return PJ_SUCCESS;
    }
    vc->enc_last_seq = hdr->seq;
    vc->enc_have_seq = PJ_TRUE;
    if (vpt.cb.capture_consumed)
        (*vpt.cb.capture_consumed)(vpt.cb.user_data, hdr->seq);

    /* Send nothing until the remote has a keyframe of this source stream
     * (see source_epoch). */
    pj_mutex_lock(vpt.lock);
    epoch = vpt.source_epoch;
    keyed = (vpt.keyed_epoch == epoch);
    pj_mutex_unlock(vpt.lock);
    if (vc->enc_epoch != epoch) {
        vc->enc_epoch = epoch;
        vc->enc_sent_key = keyed;
    }
    vc->enc_key = (hdr->flags & AU_FLAG_KEYFRAME) != 0;
    if (!vc->enc_sent_key && !vc->enc_key) {
        no_output(output, input, has_more);
        return PJ_SUCCESS;
    }
    vc->enc_sent_key = PJ_TRUE;

    pj_memcpy(vc->enc_buf, (const pj_uint8_t*)input->buf + AU_HDR_SIZE,
              hdr->len);
    vc->enc_len = hdr->len;
    vc->enc_pos = 0;
    output->timestamp = input->timestamp;

    /* Before the first packet goes out, so a receiver never gets part of
     * an access unit.
     */
    if (vc->enc_single_nal &&
        largest_nal(vc->enc_buf, vc->enc_len) > vc->prm->enc_mtu)
    {
        drop_oversized(vc, output, has_more);
        return PJ_SUCCESS;
    }
    if (vc->enc_key)
        set_keyed(epoch);

    return vc_encode_more(codec, out_size, output, has_more);
}

static pj_status_t vc_decode(pjmedia_vid_codec *codec,
                             pj_size_t count,
                             pjmedia_frame packets[],
                             unsigned out_size,
                             pjmedia_frame *output)
{
    vpt_codec *vc;
    unsigned whole_len = 0;
    unsigned i;
    pj_status_t status;
    au_hdr *hdr;

    PJ_ASSERT_RETURN(codec && count && packets && output && output->buf,
                     PJ_EINVAL);
    vc = (vpt_codec*)codec->codec_data;

    for (i = 0; i < count; ++i) {
        status = pjmedia_h264_unpacketize(vc->pktz,
                                          (pj_uint8_t*)packets[i].buf,
                                          packets[i].size,
                                          vc->dec_buf, vc->dec_buf_size,
                                          &whole_len);
        if (status != PJ_SUCCESS) {
            PJ_PERROR(4,(THIS_FILE, status, "Unpacketize error"));
            continue;
        }
    }

    if (whole_len == 0 || out_size < AU_HDR_SIZE + whole_len) {
        if (whole_len != 0) {
            PJ_LOG(4,(THIS_FILE, "Access unit of %u bytes exceeds frame",
                      whole_len));
        }
        output->type = PJMEDIA_FRAME_TYPE_NONE;
        output->size = 0;
        output->timestamp = packets[0].timestamp;
        return PJ_SUCCESS;
    }

    hdr = (au_hdr*)output->buf;
    hdr->magic = AU_MAGIC;
    hdr->seq = ++vc->dec_seq;
    hdr->len = whole_len;
    hdr->flags = is_keyframe(vc->dec_buf, whole_len) ? AU_FLAG_KEYFRAME : 0;

    /* Every reassembled access unit, here rather than at the render device
     * (see the top of this file). */
    if (vpt.cb.render_push) {
        (*vpt.cb.render_push)(vpt.cb.user_data, vc->dec_buf, whole_len,
                              (hdr->flags & AU_FLAG_KEYFRAME) ? 1 : 0);
    }
    pj_memcpy((pj_uint8_t*)output->buf + AU_HDR_SIZE, vc->dec_buf, whole_len);

    output->type = PJMEDIA_FRAME_TYPE_VIDEO;
    output->size = vpt.framebytes;
    output->timestamp = packets[0].timestamp;
    return PJ_SUCCESS;
}

static pjmedia_vid_codec_op vc_op =
{
    &vc_init,
    &vc_open,
    &vc_close,
    &vc_modify,
    &vc_get_param,
    &vc_encode_begin,
    &vc_encode_more,
    &vc_decode,
    NULL
};

static pjmedia_vid_codec_factory_op vc_factory_op =
{
    &vc_test_alloc,
    &vc_default_attr,
    &vc_enum_info,
    &vc_alloc_codec,
    &vc_dealloc_codec
};

/* ------------------------------------------------------------------------
 * Devices
 */

enum { DEV_CAPTURE, DEV_RENDER, DEV_COUNT };

typedef struct vpt_dev_factory
{
    pjmedia_vid_dev_factory base;
    pjmedia_vid_dev_info    info[DEV_COUNT];
} vpt_dev_factory;

typedef struct vpt_stream
{
    pjmedia_vid_dev_stream  base;
    pjmedia_vid_dev_param   param;
    pj_pool_t              *pool;
    pj_timestamp            ts;
    unsigned                ts_inc;
} vpt_stream;

static pj_status_t vd_get_param(pjmedia_vid_dev_stream *s,
                                pjmedia_vid_dev_param *param)
{
    vpt_stream *strm = (vpt_stream*)s;
    PJ_ASSERT_RETURN(strm && param, PJ_EINVAL);
    pj_memcpy(param, &strm->param, sizeof(*param));
    return PJ_SUCCESS;
}

static pj_status_t vd_get_cap(pjmedia_vid_dev_stream *s,
                              pjmedia_vid_dev_cap cap, void *value)
{
    PJ_UNUSED_ARG(s);
    PJ_UNUSED_ARG(cap);
    PJ_UNUSED_ARG(value);
    return PJMEDIA_EVID_INVCAP;
}

static pj_status_t vd_set_cap(pjmedia_vid_dev_stream *s,
                              pjmedia_vid_dev_cap cap, const void *value)
{
    PJ_UNUSED_ARG(s);
    PJ_UNUSED_ARG(value);

    /* There is no window; accept the window-related requests PJSUA makes
     * on every renderer so they do not show up as errors.
     */
    if (cap == PJMEDIA_VID_DEV_CAP_OUTPUT_HIDE ||
        cap == PJMEDIA_VID_DEV_CAP_OUTPUT_WINDOW_FLAGS)
    {
        return PJ_SUCCESS;
    }
    return PJMEDIA_EVID_INVCAP;
}

static pj_status_t vd_start(pjmedia_vid_dev_stream *s)
{
    PJ_UNUSED_ARG(s);
    return PJ_SUCCESS;
}

static pj_status_t vd_get_frame(pjmedia_vid_dev_stream *s,
                                pjmedia_frame *frame)
{
    vpt_stream *strm = (vpt_stream*)s;
    au_hdr *hdr;
    pj_size_t len = 0;
    pj_uint32_t id = 0;
    int key = 0;

    frame->type = PJMEDIA_FRAME_TYPE_VIDEO;
    frame->bit_info = 0;
    frame->timestamp = strm->ts;
    strm->ts.u64 += strm->ts_inc;

    if (frame->size < vpt.framebytes)
        return PJMEDIA_EVID_BADFORMAT;

    hdr = (au_hdr*)frame->buf;
    if (vpt.cb.capture_peek) {
        len = (*vpt.cb.capture_peek)(vpt.cb.user_data,
                                     (pj_uint8_t*)frame->buf + AU_HDR_SIZE,
                                     vpt.cfg.max_frame_size, &key, &id);
        if (len > vpt.cfg.max_frame_size || id == 0)
            len = 0;
    }

    hdr->magic = AU_MAGIC;
    hdr->len = (pj_uint32_t)len;
    hdr->flags = key ? AU_FLAG_KEYFRAME : 0;
    hdr->seq = len ? id : 0;
    frame->size = vpt.framebytes;
    return PJ_SUCCESS;
}

static pj_status_t vd_put_frame(pjmedia_vid_dev_stream *s,
                                const pjmedia_frame *frame)
{
    /* The decoder has already delivered it. */
    PJ_UNUSED_ARG(s);
    PJ_UNUSED_ARG(frame);
    return PJ_SUCCESS;
}

static pj_status_t vd_stop(pjmedia_vid_dev_stream *s)
{
    PJ_UNUSED_ARG(s);
    return PJ_SUCCESS;
}

static pj_status_t vd_destroy(pjmedia_vid_dev_stream *s)
{
    vpt_stream *strm = (vpt_stream*)s;
    PJ_ASSERT_RETURN(strm, PJ_EINVAL);
    pj_pool_release(strm->pool);
    return PJ_SUCCESS;
}

static pjmedia_vid_dev_stream_op vd_stream_op =
{
    &vd_get_param,
    &vd_get_cap,
    &vd_set_cap,
    &vd_start,
    &vd_get_frame,
    &vd_put_frame,
    &vd_stop,
    &vd_destroy
};

/* Called with vpt.lock held, or before anything else can see the info. */
static void fill_dev_info(pjmedia_vid_dev_info *info, const char *name,
                          pjmedia_dir dir)
{
    pj_bzero(info, sizeof(*info));
    pj_ansi_strxcpy(info->name, name, sizeof(info->name));
    pj_ansi_strxcpy(info->driver, DRIVER_NAME, sizeof(info->driver));
    info->dir = dir;
    info->has_callback = PJ_FALSE;
    info->caps = PJMEDIA_VID_DEV_CAP_FORMAT;
    info->fmt_cnt = 1;
    pjmedia_format_init_video(&info->fmt[0], PJRS_FORMAT_AUH4,
                              vpt.cfg.width, vpt.cfg.height,
                              vpt.cfg.poll_fps, 1);
}

static pj_status_t vd_factory_init(pjmedia_vid_dev_factory *f)
{
    vpt_dev_factory *df = (vpt_dev_factory*)f;

    fill_dev_info(&df->info[DEV_CAPTURE], CAPTURE_DEV_NAME,
                  PJMEDIA_DIR_CAPTURE);
    fill_dev_info(&df->info[DEV_RENDER], RENDER_DEV_NAME, PJMEDIA_DIR_RENDER);
    return PJ_SUCCESS;
}

/* Runs from pjsua_destroy() (video device subsystem shutdown), after the
 * codec manager is gone. Everything this module allocated lives in vpt.pool,
 * the factory included, so this is also where the module resets for a later
 * pjsua instance in the same process.
 */
static pj_status_t vd_factory_destroy(pjmedia_vid_dev_factory *f)
{
    PJ_UNUSED_ARG(f);

    if (vpt.lock)
        pj_mutex_destroy(vpt.lock);
    vpt.lock = NULL;
    if (vpt.pool)
        pj_pool_release(vpt.pool);
    vpt.pool = NULL;
    vpt.inited = PJ_FALSE;
    return PJ_SUCCESS;
}

static pj_status_t vd_factory_refresh(pjmedia_vid_dev_factory *f)
{
    PJ_UNUSED_ARG(f);
    return PJ_SUCCESS;
}

static unsigned vd_factory_get_dev_count(pjmedia_vid_dev_factory *f)
{
    PJ_UNUSED_ARG(f);
    return DEV_COUNT;
}

static pj_status_t vd_factory_get_dev_info(pjmedia_vid_dev_factory *f,
                                           unsigned index,
                                           pjmedia_vid_dev_info *info)
{
    vpt_dev_factory *df = (vpt_dev_factory*)f;

    PJ_ASSERT_RETURN(index < DEV_COUNT, PJMEDIA_EVID_INVDEV);
    pj_mutex_lock(vpt.lock);
    pj_memcpy(info, &df->info[index], sizeof(*info));
    pj_mutex_unlock(vpt.lock);
    return PJ_SUCCESS;
}

static pj_status_t vd_factory_default_param(pj_pool_t *pool,
                                            pjmedia_vid_dev_factory *f,
                                            unsigned index,
                                            pjmedia_vid_dev_param *param)
{
    vpt_dev_factory *df = (vpt_dev_factory*)f;

    PJ_UNUSED_ARG(pool);
    PJ_ASSERT_RETURN(index < DEV_COUNT, PJMEDIA_EVID_INVDEV);

    pj_bzero(param, sizeof(*param));
    param->dir = df->info[index].dir;
    if (index == DEV_CAPTURE) {
        param->cap_id = index;
        param->rend_id = PJMEDIA_VID_INVALID_DEV;
    } else {
        param->cap_id = PJMEDIA_VID_INVALID_DEV;
        param->rend_id = index;
    }
    param->flags = PJMEDIA_VID_DEV_CAP_FORMAT;
    param->clock_rate = CLOCK_RATE;
    pj_mutex_lock(vpt.lock);
    pj_memcpy(&param->fmt, &df->info[index].fmt[0], sizeof(param->fmt));
    pj_mutex_unlock(vpt.lock);
    return PJ_SUCCESS;
}

static pj_status_t vd_factory_create_stream(pjmedia_vid_dev_factory *f,
                                            pjmedia_vid_dev_param *param,
                                            const pjmedia_vid_dev_cb *cb,
                                            void *user_data,
                                            pjmedia_vid_dev_stream **p_strm)
{
    pj_pool_t *pool;
    vpt_stream *strm;
    const pjmedia_video_format_detail *vfd;

    PJ_UNUSED_ARG(f);
    PJ_UNUSED_ARG(cb);
    PJ_UNUSED_ARG(user_data);
    PJ_ASSERT_RETURN(param && p_strm, PJ_EINVAL);

    if (param->fmt.id != PJRS_FORMAT_AUH4)
        return PJMEDIA_EVID_BADFORMAT;

    vfd = pjmedia_format_get_video_format_detail(&param->fmt, PJ_TRUE);
    if (!vfd || vfd->fps.num == 0)
        return PJMEDIA_EVID_BADFORMAT;

    pool = pj_pool_create(vpt.pf, "vptdev%p", 512, 512, NULL);
    if (!pool)
        return PJ_ENOMEM;

    strm = PJ_POOL_ZALLOC_T(pool, vpt_stream);
    pj_memcpy(&strm->param, param, sizeof(*param));
    strm->pool = pool;
    strm->ts_inc = PJMEDIA_SPF2(param->clock_rate, &vfd->fps, 1);
    strm->base.op = &vd_stream_op;

    *p_strm = &strm->base;
    return PJ_SUCCESS;
}

static pjmedia_vid_dev_factory_op vd_factory_op =
{
    &vd_factory_init,
    &vd_factory_destroy,
    &vd_factory_get_dev_count,
    &vd_factory_get_dev_info,
    &vd_factory_default_param,
    &vd_factory_create_stream,
    &vd_factory_refresh
};

/* ------------------------------------------------------------------------
 * Public API
 */

static pj_bool_t cfg_valid(const pjrs_vpt_config *cfg)
{
    return cfg->width && cfg->height && cfg->fps && cfg->poll_fps &&
           cfg->max_frame_size && cfg->packetization_mode <= 1;
}

/* The fmtp strings derived from cfg. */
static void set_fmtp_strings(const pjrs_vpt_config *cfg)
{
    pj_ansi_snprintf(vpt.profile_level_id, sizeof(vpt.profile_level_id),
                     "%02x%02x%02x", cfg->profile_idc, cfg->profile_iop,
                     cfg->level_idc);
    pj_ansi_snprintf(vpt.max_fs, sizeof(vpt.max_fs), "%u", cfg->max_fs);
    pj_ansi_snprintf(vpt.max_mbps, sizeof(vpt.max_mbps), "%u", cfg->max_mbps);
    pj_ansi_snprintf(vpt.max_br, sizeof(vpt.max_br), "%u", cfg->max_br);
}

pj_status_t pjrs_vpt_init(const pjrs_vpt_config *cfg,
                          const pjrs_vpt_callbacks *cb,
                          pjrs_vpt_devices *devs)
{
    static const pj_str_t h264_name = { (char*)"H264", 4 };
    pjmedia_vid_codec_mgr *mgr;
    vpt_dev_factory *df;
    pj_status_t status;

    PJ_ASSERT_RETURN(cfg && cb && devs, PJ_EINVAL);
    PJ_ASSERT_RETURN(!vpt.inited, PJ_EINVALIDOP);
    PJ_ASSERT_RETURN(cfg_valid(cfg), PJ_EINVAL);

    mgr = pjmedia_vid_codec_mgr_instance();
    PJ_ASSERT_RETURN(mgr, PJ_EINVALIDOP);

    pj_bzero(&vpt, sizeof(vpt));
    vpt.cfg = *cfg;
    vpt.cb = *cb;
    vpt.pf = pjsua_get_pool_factory();
    vpt.framebytes = AU_HDR_SIZE + cfg->max_frame_size;
    vpt.source_epoch = 1;
    set_fmtp_strings(cfg);

    vpt.pool = pj_pool_create(vpt.pf, "vpt", 1024, 1024, NULL);
    if (!vpt.pool)
        return PJ_ENOMEM;
    status = pj_mutex_create_simple(vpt.pool, "vpt", &vpt.lock);
    if (status != PJ_SUCCESS)
        goto on_error;

    /* Format */
    vpt.vfi.id = PJRS_FORMAT_AUH4;
    pj_ansi_strxcpy(vpt.vfi.name, "AUH4", sizeof(vpt.vfi.name));
    vpt.vfi.color_model = PJMEDIA_COLOR_MODEL_NONE;
    vpt.vfi.bpp = 8;
    vpt.vfi.plane_cnt = 1;
    vpt.vfi.apply_fmt = &auh4_apply_fmt;
    status = pjmedia_register_video_format_info(NULL, &vpt.vfi);
    if (status != PJ_SUCCESS)
        goto on_error;

    /* Codec. The fmt-match callback is shared with any other H.264 codec
     * that was compiled in, which may already have registered it.
     */
    status = pjmedia_sdp_neg_register_fmt_match_cb(
                                        &h264_name,
                                        &pjmedia_vid_codec_h264_match_sdp);
    if (status != PJ_SUCCESS && status != PJ_EEXISTS)
        goto on_error;

    vpt.codec_factory.op = &vc_factory_op;
    status = pjmedia_vid_codec_mgr_register_factory(mgr, &vpt.codec_factory);
    if (status != PJ_SUCCESS)
        goto on_error;

    /* Devices */
    df = PJ_POOL_ZALLOC_T(vpt.pool, vpt_dev_factory);
    df->base.op = &vd_factory_op;
    vpt.dev_factory = &df->base;
    status = pjmedia_vid_register_factory(NULL, &df->base);
    if (status != PJ_SUCCESS) {
        pjmedia_vid_codec_mgr_unregister_factory(mgr, &vpt.codec_factory);
        goto on_error;
    }

    status = pjmedia_vid_dev_lookup(DRIVER_NAME, CAPTURE_DEV_NAME,
                                    &devs->capture);
    if (status == PJ_SUCCESS)
        status = pjmedia_vid_dev_lookup(DRIVER_NAME, RENDER_DEV_NAME,
                                        &devs->render);
    if (status != PJ_SUCCESS) {
        pjmedia_vid_unregister_factory(NULL, &df->base);
        pjmedia_vid_codec_mgr_unregister_factory(mgr, &vpt.codec_factory);
        goto on_error;
    }

    vpt.inited = PJ_TRUE;
    PJ_LOG(4,(THIS_FILE, "H.264 passthrough ready: %ux%u@%u, pt=%u, "
              "capture dev %d, render dev %d", cfg->width, cfg->height,
              cfg->fps, cfg->pt, devs->capture, devs->render));
    return PJ_SUCCESS;

on_error:
    if (vpt.lock)
        pj_mutex_destroy(vpt.lock);
    vpt.lock = NULL;
    pj_pool_release(vpt.pool);
    vpt.pool = NULL;
    return status;
}

pj_status_t pjrs_vpt_reconfigure(const pjrs_vpt_config *cfg)
{
    pjmedia_vid_codec_mgr *mgr;
    vpt_dev_factory *df;
    pj_status_t status;

    /* Caller input, so refused rather than asserted. */
    if (!vpt.inited)
        return PJ_EINVALIDOP;
    if (!cfg || !cfg_valid(cfg))
        return PJ_EINVAL;
    /* Both size buffers and match payloads of streams that may be open. */
    if (cfg->max_frame_size != vpt.cfg.max_frame_size || cfg->pt != vpt.cfg.pt)
        return PJ_EINVAL;

    mgr = pjmedia_vid_codec_mgr_instance();
    PJ_ASSERT_RETURN(mgr, PJ_EINVALIDOP);
    df = (vpt_dev_factory*)vpt.dev_factory;

    /* Re-register so the codec manager drops what it enumerated from the
     * old parameters. */
    status = pjmedia_vid_codec_mgr_unregister_factory(mgr, &vpt.codec_factory);
    if (status != PJ_SUCCESS)
        return status;

    pj_mutex_lock(vpt.lock);
    vpt.cfg = *cfg;
    set_fmtp_strings(cfg);
    fill_dev_info(&df->info[DEV_CAPTURE], CAPTURE_DEV_NAME,
                  PJMEDIA_DIR_CAPTURE);
    fill_dev_info(&df->info[DEV_RENDER], RENDER_DEV_NAME, PJMEDIA_DIR_RENDER);
    pj_mutex_unlock(vpt.lock);

    status = pjmedia_vid_codec_mgr_register_factory(mgr, &vpt.codec_factory);
    if (status != PJ_SUCCESS) {
        PJ_PERROR(1,(THIS_FILE, status, "H.264 passthrough could not "
                     "re-register; video is unavailable"));
        return status;
    }
    PJ_LOG(4,(THIS_FILE, "H.264 passthrough reconfigured: %ux%u@%u, "
              "%u/%u bps, packetization-mode %u", cfg->width, cfg->height,
              cfg->fps, cfg->avg_bps, cfg->max_bps, cfg->packetization_mode));
    return PJ_SUCCESS;
}

pj_status_t pjrs_vid_remote_caps_get(pjsua_call_id call_id, unsigned med_idx,
                                     pjrs_vid_remote_caps *caps)
{
    static const pj_str_t PLI = { (char*)"profile-level-id", 16 };
    static const pj_str_t TIAS = { (char*)"TIAS", 4 };
    static const pj_str_t AS = { (char*)"AS", 2 };
    pjsua_stream_info si;
    const pjmedia_vid_codec_param *prm;
    pjmedia_vid_codec_h264_fmtp fmtp;
    const pjmedia_sdp_session *rem = NULL;
    pjsua_call *call;
    pj_status_t status;
    unsigned i;

    PJ_ASSERT_RETURN(caps, PJ_EINVAL);
    if (call_id < 0 || call_id >= (int)pjsua_var.ua_cfg.max_calls)
        return PJ_EINVAL;
    pj_bzero(caps, sizeof(*caps));

    /* The remote's fmtp is the one our encoder works to. */
    status = pjsua_call_get_stream_info(call_id, med_idx, &si);
    if (status != PJ_SUCCESS)
        return status;
    if (si.type != PJMEDIA_TYPE_VIDEO || !si.info.vid.codec_param)
        return PJ_ENOTFOUND;
    prm = si.info.vid.codec_param;

    for (i = 0; i < prm->enc_fmtp.cnt; ++i) {
        if (!pj_stricmp(&prm->enc_fmtp.param[i].name, &PLI))
            caps->has_profile_level_id = 1;
    }
    if (pjmedia_vid_codec_h264_parse_fmtp(&prm->enc_fmtp, &fmtp) == PJ_SUCCESS) {
        caps->profile_idc = fmtp.profile_idc;
        caps->profile_iop = fmtp.profile_iop;
        caps->level_idc = fmtp.level;
        caps->packetization_mode = fmtp.packetization_mode;
        caps->max_fs = fmtp.max_fs;
        caps->max_mbps = fmtp.max_mbps;
        caps->max_br = fmtp.max_br;
    }

    /* Bandwidth is not in the stream info; read the active remote SDP. */
    PJSUA_LOCK();
    call = &pjsua_var.calls[call_id];
    if (call->inv && call->inv->neg &&
        pjmedia_sdp_neg_get_active_remote(call->inv->neg, &rem) == PJ_SUCCESS &&
        rem && med_idx < rem->media_count)
    {
        const pjmedia_sdp_media *m = rem->media[med_idx];
        for (i = 0; i < m->bandw_count; ++i) {
            if (!pj_stricmp(&m->bandw[i]->modifier, &TIAS))
                caps->tias_bps = m->bandw[i]->value;
            else if (!pj_stricmp(&m->bandw[i]->modifier, &AS))
                caps->as_kbps = m->bandw[i]->value;
        }
    }
    PJSUA_UNLOCK();
    return PJ_SUCCESS;
}

pj_status_t pjrs_vid_set_transmit(pjsua_call_id call_id, int enable)
{
    pjsua_call_vid_strm_op_param prm;

    pjsua_call_vid_strm_op_param_default(&prm);
    prm.med_idx = -1;
    return pjsua_call_set_vid_strm(call_id,
                                   enable ? PJSUA_CALL_VID_STRM_START_TRANSMIT
                                          : PJSUA_CALL_VID_STRM_STOP_TRANSMIT,
                                   &prm);
}

#else
#  error "The video feature needs pjproject built with PJMEDIA_HAS_VIDEO=1"
#endif  /* PJMEDIA_HAS_VIDEO */

void pjrs_vpt_source_restarted(void)
{
    if (!vpt.inited)
        return;
    pj_mutex_lock(vpt.lock);
    /* Never 0, which keyed_epoch uses for "none". */
    if (++vpt.source_epoch == 0)
        vpt.source_epoch = 1;
    pj_mutex_unlock(vpt.lock);
}
