#include <pjsua-lib/pjsua.h>

/* Custom helper: update a transport's published address without closing it */
pj_status_t pjsua_transport_set_published_address(pjsua_transport_id id,
                                                    const char *addr);

/* Custom helpers: track TLS connections and drop them on a credential change */
pj_status_t pjsua_tls_tracker_install(void);
unsigned pjsua_tls_shutdown_connections(void);
