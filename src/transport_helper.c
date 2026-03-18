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
