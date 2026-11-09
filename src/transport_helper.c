/*
 * Helper to update a PJSUA transport's published address.
 *
 * PJSIP caches the transport's published address (addr_name) at creation
 * time.  After a network change (DHCP ↔ static), the cached address is
 * stale and outgoing SIP Contact headers advertise the wrong IP.
 *
 * The UDP socket itself (bound to 0.0.0.0) works fine on any interface —
 * we just need PJSIP to use the new IP for Contact header generation.
 * Closing the UDP transport is blocked by PJSIP's reference counting
 * (accounts, in-flight transactions), so we update addr_name in-place.
 */

#include <pjsua-lib/pjsua.h>
#include <pjsua-lib/pjsua_internal.h>
#include <string.h>

/**
 * Update the published address (addr_name) of a PJSUA transport.
 *
 * This modifies tp->addr_name.host so that subsequent Contact/Via headers
 * use the new IP address.  The underlying socket is NOT changed.
 *
 * @param id    PJSUA transport ID
 * @param addr  New IP address string (e.g. "192.168.10.191")
 * @return      PJ_SUCCESS on success
 */
pj_status_t pjsua_transport_set_published_address(pjsua_transport_id id,
                                                    const char *addr)
{
    pjsua_transport_info info;
    pj_status_t status;
    pjsip_transport *tp;

    /* Validate transport ID */
    status = pjsua_transport_get_info(id, &info);
    if (status != PJ_SUCCESS)
        return status;

    /* Get the raw transport pointer from pjsua internal data */
    tp = pjsua_var.tpdata[id].data.tp;
    if (!tp)
        return PJ_EINVAL;

    /* Update the published host address (local_name) */
    pj_strdup2(tp->pool, &tp->local_name.host, addr);

    PJ_LOG(3, ("tp_helper", "Transport %d published address updated to %s:%d",
               id, addr, tp->local_name.port));

    return PJ_SUCCESS;
}

/*
 * Closing a TLS listener leaves the connections it made open, and a new
 * account to the same server reuses one: the replacement listener can be
 * allocated where the old one was, so PJSIP's listener match still passes.
 * A credential change must not ride on a connection authenticated with the
 * old ones, so every TLS connection is tracked here and can be shut down.
 */
#define MAX_TLS_CONNS 64

static pjsip_transport *tls_conns[MAX_TLS_CONNS];
static pj_mutex_t *tls_conns_lock;
static pj_pool_t *tls_conns_pool;
static void (*prev_on_transport_state)(pjsip_transport *,
                                       pjsip_transport_state,
                                       const pjsip_transport_state_info *);

static void tls_tracker_on_transport_state(pjsip_transport *tp,
                                           pjsip_transport_state state,
                                           const pjsip_transport_state_info *info)
{
    if ((tp->key.type & ~PJSIP_TRANSPORT_IPV6) == PJSIP_TRANSPORT_TLS &&
        tp->grp_lock &&
        (state == PJSIP_TP_STATE_CONNECTED || state == PJSIP_TP_STATE_DESTROY))
    {
        unsigned i, free_slot = MAX_TLS_CONNS;

        pj_mutex_lock(tls_conns_lock);
        for (i = 0; i < MAX_TLS_CONNS; ++i) {
            if (tls_conns[i] == tp) {
                if (state == PJSIP_TP_STATE_DESTROY)
                    tls_conns[i] = NULL;
                break;
            }
            if (!tls_conns[i] && free_slot == MAX_TLS_CONNS)
                free_slot = i;
        }
        if (i == MAX_TLS_CONNS && state == PJSIP_TP_STATE_CONNECTED) {
            if (free_slot < MAX_TLS_CONNS)
                tls_conns[free_slot] = tp;
            else
                PJ_LOG(2, ("tp_helper", "TLS connection table full, %s untracked",
                           tp->obj_name));
        }
        pj_mutex_unlock(tls_conns_lock);
    }

    if (prev_on_transport_state)
        (*prev_on_transport_state)(tp, state, info);
}

/**
 * Start tracking TLS connections. Call after pjsua_init().
 */
pj_status_t pjsua_tls_tracker_install(void)
{
    pj_status_t status;

    /* A new pjsua instance has a new pool; the old lock went with the old one. */
    if (tls_conns_lock && tls_conns_pool == pjsua_var.pool)
        return PJ_SUCCESS;

    tls_conns_lock = NULL;
    pj_bzero(tls_conns, sizeof(tls_conns));
    status = pj_mutex_create_simple(pjsua_var.pool, "tls_conns", &tls_conns_lock);
    if (status != PJ_SUCCESS)
        return status;
    tls_conns_pool = pjsua_var.pool;

    prev_on_transport_state = pjsua_var.ua_cfg.cb.on_transport_state;
    pjsua_var.ua_cfg.cb.on_transport_state = &tls_tracker_on_transport_state;
    return PJ_SUCCESS;
}

/**
 * Shut down every open TLS connection so the next request opens a new one.
 *
 * @return  The number of connections shut down.
 */
unsigned pjsua_tls_shutdown_connections(void)
{
    pjsip_transport *conns[MAX_TLS_CONNS];
    unsigned i, count = 0;

    if (!tls_conns_lock)
        return 0;

    /* The group lock ref keeps each transport's memory alive once the table
     * lock is dropped; shutdown2 itself takes the transport manager lock. */
    pj_mutex_lock(tls_conns_lock);
    for (i = 0; i < MAX_TLS_CONNS; ++i) {
        if (tls_conns[i]) {
            pj_grp_lock_add_ref(tls_conns[i]->grp_lock);
            conns[count++] = tls_conns[i];
        }
    }
    pj_mutex_unlock(tls_conns_lock);

    for (i = 0; i < count; ++i) {
        pjsip_transport_shutdown2(conns[i], PJ_TRUE);
        pj_grp_lock_dec_ref(conns[i]->grp_lock);
    }
    return count;
}
