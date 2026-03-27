/*
 * PJSIP module that intercepts ALL SIP messages (requests and responses)
 * for tracing purposes. Unlike on_call_tsx_state which only fires on
 * transaction state changes, this module sees every SIP message passing
 * through the stack.
 *
 * Each intercepted message is forwarded to a Rust callback via FFI.
 * The module never consumes messages — it returns PJ_FALSE/PJ_SUCCESS
 * so normal PJSIP processing continues.
 */

#include <pjsua-lib/pjsua.h>

/* Rust callback — defined in event.rs as #[no_mangle] extern "C" */
extern void rust_sip_trace_on_msg(
    int         is_outgoing,
    const char *method_or_status,
    int         method_or_status_len,
    const char *sip_call_id,
    int         sip_call_id_len,
    const char *sdp_body,
    int         sdp_body_len
);

/* ---------- helpers ---------- */

static void extract_and_trace(pjsip_msg *msg, int is_outgoing)
{
    /* Method or status string */
    char mos_buf[128];
    int  mos_len;

    if (msg->type == PJSIP_REQUEST_MSG) {
        pj_str_t *name = &msg->line.req.method.name;
        mos_len = (int)PJ_MIN(name->slen, (pj_ssize_t)(sizeof(mos_buf) - 1));
        pj_memcpy(mos_buf, name->ptr, mos_len);
        mos_buf[mos_len] = '\0';
    } else {
        mos_len = pj_ansi_snprintf(mos_buf, sizeof(mos_buf), "%d %.*s",
                                   msg->line.status.code,
                                   (int)msg->line.status.reason.slen,
                                   msg->line.status.reason.ptr);
        if (mos_len < 0) mos_len = 0;
    }

    /* Call-ID header */
    const char *cid_ptr = NULL;
    int         cid_len = 0;
    pjsip_cid_hdr *cid_hdr = (pjsip_cid_hdr *)
        pjsip_msg_find_hdr(msg, PJSIP_H_CALL_ID, NULL);
    if (cid_hdr) {
        cid_ptr = cid_hdr->id.ptr;
        cid_len = (int)cid_hdr->id.slen;
    }

    /* SDP body (if present) */
    const char *sdp_ptr = NULL;
    int         sdp_len = 0;
    if (msg->body && msg->body->print_body) {
        char sdp_buf[4096];
        int n = msg->body->print_body(msg->body, sdp_buf, sizeof(sdp_buf));
        if (n > 0) {
            sdp_ptr = sdp_buf;
            sdp_len = n;
        }
    }

    rust_sip_trace_on_msg(is_outgoing,
                          mos_buf, mos_len,
                          cid_ptr, cid_len,
                          sdp_ptr, sdp_len);
}

/* ---------- module callbacks ---------- */

static pj_bool_t trace_on_rx_request(pjsip_rx_data *rdata)
{
    extract_and_trace(rdata->msg_info.msg, 0);
    return PJ_FALSE;          /* don't consume — let other modules process */
}

static pj_bool_t trace_on_rx_response(pjsip_rx_data *rdata)
{
    extract_and_trace(rdata->msg_info.msg, 0);
    return PJ_FALSE;
}

static pj_status_t trace_on_tx_request(pjsip_tx_data *tdata)
{
    extract_and_trace(tdata->msg, 1);
    return PJ_SUCCESS;
}

static pj_status_t trace_on_tx_response(pjsip_tx_data *tdata)
{
    extract_and_trace(tdata->msg, 1);
    return PJ_SUCCESS;
}

/* ---------- module definition ---------- */

static pjsip_module mod_sip_trace = {
    NULL, NULL,                              /* prev, next */
    { "mod-sip-trace", 13 },                /* name        */
    -1,                                      /* id          */
    PJSIP_MOD_PRIORITY_TRANSPORT_LAYER + 1,  /* priority    */
    NULL,                                    /* load        */
    NULL,                                    /* start       */
    NULL,                                    /* stop        */
    NULL,                                    /* unload      */
    &trace_on_rx_request,                    /* on_rx_request  */
    &trace_on_rx_response,                   /* on_rx_response */
    &trace_on_tx_request,                    /* on_tx_request  */
    &trace_on_tx_response,                   /* on_tx_response */
    NULL                                     /* on_tsx_state   */
};

/* ---------- public API (called from Rust) ---------- */

pj_status_t sip_trace_module_register(void)
{
    pjsip_endpoint *endpt = pjsua_get_pjsip_endpt();
    if (!endpt) return PJ_EINVAL;
    return pjsip_endpt_register_module(endpt, &mod_sip_trace);
}

void sip_trace_module_unregister(void)
{
    pjsip_endpoint *endpt = pjsua_get_pjsip_endpt();
    if (endpt) {
        pjsip_endpt_unregister_module(endpt, &mod_sip_trace);
    }
}
