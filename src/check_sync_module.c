/*
 * PJSIP module that answers an out-of-dialog `check-sync` NOTIFY from the
 * server an account is registered with.
 *
 * Phone systems send such a NOTIFY to tell a device to fetch its provisioning
 * again (`Event: check-sync`) or to restart (`Event: check-sync;reboot=true`).
 * They send it without credentials, so the only check there is on the sender:
 * the request is taken only when its source IP address and its transport type
 * equal those of the last successful REGISTER answer of an account that is
 * registered now. The port is not compared.
 *
 * A request that is taken is answered 200 OK statelessly and handed to Rust,
 * which raises `SipEvent::CheckSync`. Any other request is left to the other
 * modules (PJ_FALSE): a check-sync from anyone else gets no answer at all,
 * as before this module existed, and pjsua's own unsolicited MWI handler
 * still sees `Event: message-summary`.
 *
 * Modelled on pjsua-lib's unsolicited_mwi_on_rx_request() (pjsua_pres.c),
 * which does the same with no sender check.
 */

#include <pjsua-lib/pjsua.h>
#include <pthread.h>
#include "../wrapper.h"

#define THIS_FILE "check_sync_module.c"

/* Rust callback, defined in check_sync.rs as #[no_mangle] extern "C".
 *
 * Both buffers belong to the caller and are valid only for the duration of
 * the call: `source` is a stack buffer here, `reboot` points into the pool
 * of the incoming rdata. Neither is NUL-terminated as far as Rust is
 * concerned; the lengths are authoritative. Rust copies both before it
 * returns.
 */
extern void rust_check_sync_on_request(
    int         acc_id,
    const char *source,
    int         source_len,
    const char *reboot,
    int         reboot_len,
    int         has_reboot
);

/* ---------- registered-server table ---------- */

/* What the last successful REGISTER answer of one account came from. */
typedef struct reg_source
{
    pj_bool_t   valid;      /* PJ_FALSE: not registered, accept nothing */
    pj_sockaddr addr;       /* source address of the 2xx (port ignored)  */
    long        tp_type;    /* transport type, PJSIP_TRANSPORT_IPV6 off  */
} reg_source;

/* Indexed by pjsua account id.
 *
 * on_reg_state2 and on_rx_request can run on different PJSIP threads, so
 * every access holds `reg_sources_lock`. A plain static pthread mutex rather
 * than a pj_mutex_t: it needs no pool, so it stays usable while pjsua_destroy
 * unregisters the accounts and their callbacks still arrive. The lock is a
 * leaf: nothing else is locked, and nothing is called into PJSIP or Rust,
 * while it is held.
 */
static reg_source      reg_sources[PJSUA_MAX_ACC];
static pthread_mutex_t reg_sources_lock = PTHREAD_MUTEX_INITIALIZER;

/* pj_sockaddr_get_addr() and pj_sockaddr_get_addr_len() assert on any other
 * family (and asserts are compiled in on the devices), so every address is
 * checked with this before it reaches them.
 */
static pj_bool_t is_ip_family(const pj_sockaddr *addr)
{
    return addr->addr.sa_family == pj_AF_INET() ||
           addr->addr.sa_family == pj_AF_INET6();
}

/* Same family and same address bytes. The port is not compared. */
static pj_bool_t same_ip(const pj_sockaddr *a, const pj_sockaddr *b)
{
    if (!is_ip_family(a) || a->addr.sa_family != b->addr.sa_family)
        return PJ_FALSE;
    return pj_memcmp(pj_sockaddr_get_addr(a), pj_sockaddr_get_addr(b),
                     pj_sockaddr_get_addr_len(a)) == 0;
}

/* UDP, TCP or TLS, the same for IPv4 and IPv6 (the family is compared on
 * its own in same_ip()).
 */
static long base_tp_type(const pjsip_transport *tp)
{
    return tp->key.type & ~(long)PJSIP_TRANSPORT_IPV6;
}

static void set_reg_source(pjsua_acc_id acc_id, const reg_source *rec)
{
    pthread_mutex_lock(&reg_sources_lock);
    reg_sources[acc_id] = *rec;
    pthread_mutex_unlock(&reg_sources_lock);
}

/* The account whose registered server sent this request, or
 * PJSUA_INVALID_ID.
 */
static pjsua_acc_id find_registered_account(const pjsip_rx_data *rdata)
{
    const pj_sockaddr *src = &rdata->pkt_info.src_addr;
    pjsua_acc_id found = PJSUA_INVALID_ID;
    long tp_type;
    unsigned i;

    if (!rdata->tp_info.transport)
        return PJSUA_INVALID_ID;
    tp_type = base_tp_type(rdata->tp_info.transport);

    pthread_mutex_lock(&reg_sources_lock);
    for (i = 0; i < PJ_ARRAY_SIZE(reg_sources); ++i) {
        const reg_source *rec = &reg_sources[i];
        if (rec->valid && rec->tp_type == tp_type && same_ip(&rec->addr, src)) {
            found = (pjsua_acc_id)i;
            break;
        }
    }
    pthread_mutex_unlock(&reg_sources_lock);

    return found;
}

/* ---------- module callbacks ---------- */

static pj_bool_t check_sync_on_rx_request(pjsip_rx_data *rdata)
{
    static const pj_str_t EVENT_HDR       = { "Event", 5 };
    static const pj_str_t EVENT_HDR_SHORT = { "o", 1 };
    static const pj_str_t REBOOT_PARAM    = { "reboot", 6 };

    pjsip_msg *msg = rdata->msg_info.msg;
    pjsip_event_hdr *event;
    const pjsip_param *reboot;
    pjsua_acc_id acc_id;
    char source[PJ_INET6_ADDRSTRLEN];
    pj_status_t status;

    /* Out-of-dialog NOTIFY only. A To tag means a dialog, which is the
     * dialog and event-subscription layers' business.
     */
    if (pjsip_method_cmp(&msg->line.req.method, pjsip_get_notify_method()) != 0)
        return PJ_FALSE;
    if (!rdata->msg_info.to || rdata->msg_info.to->tag.slen != 0)
        return PJ_FALSE;

    /* The cast relies on the evsub header parser being installed, which
     * pjsua_init() does: pjsua's own unsolicited MWI handler (on by default)
     * makes the same cast on the same out-of-dialog NOTIFYs.
     */
    event = (pjsip_event_hdr *)
        pjsip_msg_find_hdr_by_names(msg, &EVENT_HDR, &EVENT_HDR_SHORT, NULL);
    if (!event || pj_stricmp2(&event->event_type, "check-sync") != 0)
        return PJ_FALSE;

    if (!is_ip_family(&rdata->pkt_info.src_addr)) {
        PJ_LOG(5, (THIS_FILE, "check-sync refused: source is not an IP address"));
        return PJ_FALSE;
    }
    /* Empty, not garbage, if pj_sockaddr_print() fails without writing. */
    source[0] = '\0';
    pj_sockaddr_print(&rdata->pkt_info.src_addr, source, sizeof(source), 0);

    acc_id = find_registered_account(rdata);
    if (acc_id == PJSUA_INVALID_ID) {
        PJ_LOG(5, (THIS_FILE, "check-sync from %s refused: not the registered "
                   "server of a registered account over this transport",
                   source));
        return PJ_FALSE;
    }

    /* Answer first, then act. A failed send is logged and the request is
     * still acted on: it came from the registered server, and the server's
     * retransmission (if any) is answered the same way.
     */
    status = pjsip_endpt_respond_stateless(pjsua_get_pjsip_endpt(), rdata,
                                           200, NULL, NULL, NULL);
    if (status != PJ_SUCCESS) {
        PJ_PERROR(2, (THIS_FILE, status, "check-sync from %s: 200 OK not sent",
                      source));
    }

    reboot = pjsip_param_find(&event->other_param, &REBOOT_PARAM);
    rust_check_sync_on_request(acc_id,
                               source, (int)pj_ansi_strlen(source),
                               reboot ? reboot->value.ptr : NULL,
                               reboot ? (int)reboot->value.slen : 0,
                               reboot != NULL);
    return PJ_TRUE;
}

/* ---------- module definition ---------- */

static pjsip_module mod_check_sync = {
    NULL, NULL,                              /* prev, next */
    { "mod-check-sync", 14 },               /* name        */
    -1,                                      /* id          */
    PJSIP_MOD_PRIORITY_APPLICATION,          /* priority    */
    NULL,                                    /* load        */
    NULL,                                    /* start       */
    NULL,                                    /* stop        */
    NULL,                                    /* unload      */
    &check_sync_on_rx_request,               /* on_rx_request  */
    NULL,                                    /* on_rx_response */
    NULL,                                    /* on_tx_request  */
    NULL,                                    /* on_tx_response */
    NULL                                     /* on_tsx_state   */
};

/* ---------- public API (called from Rust) ---------- */

pj_status_t check_sync_module_register(void)
{
    pjsip_endpoint *endpt = pjsua_get_pjsip_endpt();
    if (!endpt) return PJ_EINVAL;
    return pjsip_endpt_register_module(endpt, &mod_check_sync);
}

void check_sync_module_unregister(void)
{
    pjsip_endpoint *endpt = pjsua_get_pjsip_endpt();

    if (endpt) {
        pjsip_endpt_unregister_module(endpt, &mod_check_sync);
    }

    /* The table belongs to this PJSUA instance; a later one starts empty. */
    pthread_mutex_lock(&reg_sources_lock);
    pj_bzero(reg_sources, sizeof(reg_sources));
    pthread_mutex_unlock(&reg_sources_lock);
}

void check_sync_on_reg_state(pjsua_acc_id acc_id, const pjsua_reg_info *info)
{
    const struct pjsip_regc_cbparam *cbparam;
    const pjsip_rx_data *rdata;
    reg_source rec;

    if (acc_id < 0 || acc_id >= (pjsua_acc_id)PJ_ARRAY_SIZE(reg_sources))
        return;

    pj_bzero(&rec, sizeof(rec));

    /* cbparam and rdata are NULL on a timeout or a transport error. Only a
     * REGISTER (renew) answered 2xx without error, with an expiration and
     * not marked as an unregistration, records the server; any other
     * outcome leaves `rec` invalid and so forgets the account.
     */
    cbparam = info ? info->cbparam : NULL;
    rdata = cbparam ? cbparam->rdata : NULL;
    if (info && info->renew &&
        rdata &&
        cbparam->status == PJ_SUCCESS &&
        cbparam->code / 100 == 2 &&
        cbparam->expiration > 0 &&
        !cbparam->is_unreg &&
        rdata->tp_info.transport &&
        is_ip_family(&rdata->pkt_info.src_addr) &&
        pjsua_acc_is_valid(acc_id))
    {
        rec.valid = PJ_TRUE;
        pj_memcpy(&rec.addr, &rdata->pkt_info.src_addr, sizeof(rec.addr));
        rec.tp_type = base_tp_type(rdata->tp_info.transport);
    }

    set_reg_source(acc_id, &rec);
}

void check_sync_forget_account(pjsua_acc_id acc_id)
{
    static const reg_source none;

    if (acc_id < 0 || acc_id >= (pjsua_acc_id)PJ_ARRAY_SIZE(reg_sources))
        return;
    set_reg_source(acc_id, &none);
}
