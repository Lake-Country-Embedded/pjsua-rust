#include <pjsua-lib/pjsua.h>

/* Custom helper: update a transport's published address without closing it */
pj_status_t pjsua_transport_set_published_address(pjsua_transport_id id,
                                                    const char *addr);

/* Custom helpers: track TLS connections and drop them on a credential change */
pj_status_t pjsua_tls_tracker_install(void);
unsigned pjsua_tls_shutdown_connections(void);

/* Custom helpers: answer a check-sync NOTIFY from the registered server
 * (src/check_sync_module.c). */
pj_status_t check_sync_module_register(void);
void check_sync_module_unregister(void);
void check_sync_on_reg_state(pjsua_acc_id acc_id, const pjsua_reg_info *info);
void check_sync_forget_account(pjsua_acc_id acc_id);
