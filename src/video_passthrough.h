/*
 * H.264 passthrough for PJSUA video. See video_passthrough.c.
 */
#ifndef PJSUA_RUST_VIDEO_PASSTHROUGH_H
#define PJSUA_RUST_VIDEO_PASSTHROUGH_H

#include <pjmedia/types.h>
#include <pjmedia-videodev/videodev.h>
#include <pjsua-lib/pjsua.h>

PJ_BEGIN_DECL

typedef struct pjrs_vpt_config
{
    /* Picture size and nominal frame rate of the source, as advertised. */
    unsigned    width;
    unsigned    height;
    unsigned    fps;
    /* Rate PJSUA polls for a new access unit; above fps (e.g. twice) so a
     * backlog drains. Not advertised; RTP timestamps follow real time. */
    unsigned    poll_fps;
    /* Largest access unit accepted or delivered, in bytes. */
    unsigned    max_frame_size;
    /* Bitrate hints for SDP and send pacing, in bits per second. */
    unsigned    avg_bps;
    unsigned    max_bps;
    /* RTP payload type. */
    unsigned    pt;
    /* packetization-mode advertised in SDP: 1 (FU-A allowed) or 0 (one
     * NAL unit per packet; the source must keep NAL units under the MTU).
     * Either is accepted from the remote. */
    unsigned    packetization_mode;
    /* profile-level-id advertised in SDP. */
    pj_uint8_t  profile_idc;
    pj_uint8_t  profile_iop;
    pj_uint8_t  level_idc;
    /* Receive limits advertised in SDP beyond what the level implies
     * (RFC 6184 max-fs, max-mbps, max-br); 0 leaves each out. */
    unsigned    max_fs;
    unsigned    max_mbps;
    unsigned    max_br;
} pjrs_vpt_config;

typedef struct pjrs_vpt_stream_info
{
    unsigned    width;
    unsigned    height;
    unsigned    fps_num;
    unsigned    fps_denum;
    unsigned    avg_bps;
    unsigned    max_bps;
} pjrs_vpt_stream_info;

/* All callbacks run on PJSIP media threads and must not block. */
typedef struct pjrs_vpt_callbacks
{
    void *user_data;

    /* Copy the oldest unsent Annex-B access unit into buf and return its
     * length, or 0 if none is ready. Set *keyframe if it is a refresh point
     * and *id to a nonzero number identifying it. The capture device may be
     * polled several times before the frame is sent; keep returning the
     * same one until capture_consumed() names it. */
    pj_size_t (*capture_peek)(void *user_data, pj_uint8_t *buf,
                              pj_size_t buf_size, int *keyframe,
                              pj_uint32_t *id);

    /* Access units up to and including id have been taken by the encoder. */
    void (*capture_consumed)(void *user_data, pj_uint32_t id);

    /* The remote (or the stream) wants a keyframe as soon as possible. */
    void (*keyframe_request)(void *user_data);

    /* A sending stream opened or its parameters changed. */
    void (*encoder_open)(void *user_data, const pjrs_vpt_stream_info *info);

    /* A sending stream closed. */
    void (*encoder_close)(void *user_data);

    /* An access unit arrived from the remote: each one as it is
     * reassembled, on the stream's receive path. */
    void (*render_push)(void *user_data, const pj_uint8_t *buf,
                        pj_size_t len, int keyframe);
} pjrs_vpt_callbacks;

typedef struct pjrs_vpt_devices
{
    pjmedia_vid_dev_index capture;
    pjmedia_vid_dev_index render;
} pjrs_vpt_devices;

/* What the remote of a call's video stream can receive: its H.264 fmtp,
 * with the level's limits folded in (H.264 Table A-1), and the bandwidth
 * lines of its media section. Zero means not stated. */
typedef struct pjrs_vid_remote_caps
{
    int         has_profile_level_id;
    pj_uint8_t  profile_idc;
    pj_uint8_t  profile_iop;
    pj_uint8_t  level_idc;
    unsigned    packetization_mode;
    unsigned    max_fs;         /* macroblocks */
    unsigned    max_mbps;       /* macroblocks per second */
    unsigned    max_br;         /* kbit/s */
    unsigned    tias_bps;       /* b=TIAS */
    unsigned    as_kbps;        /* b=AS */
} pjrs_vid_remote_caps;

/* Fill caps for media med_idx of call_id, which must be a video stream
 * PJSUA has created. Safe to call from PJSUA callbacks. */
pj_status_t pjrs_vid_remote_caps_get(pjsua_call_id call_id, unsigned med_idx,
                                     pjrs_vid_remote_caps *caps);

/* Start or stop sending on call_id's first video stream, without SDP.
 * Starting reattaches the capture device PJSUA leaves detached after a local
 * hold. Call from a PJLIB-registered thread, outside PJSUA callbacks. */
pj_status_t pjrs_vid_set_transmit(pjsua_call_id call_id, int enable);

/* Register the format, codec and devices. Call after pjsua_init(). The
 * registration lasts until pjsua_destroy(). */
pj_status_t pjrs_vpt_init(const pjrs_vpt_config *cfg,
                          const pjrs_vpt_callbacks *cb,
                          pjrs_vpt_devices *devs);

/* Replace the configuration, re-registering the codec so new calls offer
 * and send with it. Streams already open are unaffected. max_frame_size and
 * pt cannot change. Call from a PJLIB-registered thread. */
pj_status_t pjrs_vpt_reconfigure(const pjrs_vpt_config *cfg);

/* The source restarted: nothing more is sent until its next keyframe.
 * Without this, a stream reopened by a re-INVITE carries on from the
 * source's last keyframe. */
void pjrs_vpt_source_restarted(void);

PJ_END_DECL

#endif
