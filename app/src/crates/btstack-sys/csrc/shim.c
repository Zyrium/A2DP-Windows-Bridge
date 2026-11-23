/*
 * shim.c - C glue between BTstack and the Rust side of btstack-sys
 *
 * Holds everything whose layout is BTstack's business: the static structs
 * BTstack keeps pointers to (registrations, timers, SDP record buffers,
 * stream endpoint capabilities, the codec configuration of
 * a2dp_source_set_config_other; for LE: the TLV store, per-connection GATT
 * client listeners and write buffers, ATT send registrations, CIG storage),
 * the function tables Rust implements (HCI transport, link key DB, HCI dump,
 * ATT server read / write callbacks), and reads of struct members
 * (hci_connection_t, avdtp_connection_t). No logic of its own beyond the
 * bookkeeping of that storage: every decision is made in Rust. Runs on the
 * BTstack run loop thread only, except bluecaster_wake().
 *
 * SPDX-License-Identifier: Apache-2.0 OR MIT
 */

#include "btstack_config.h"

#include <stdarg.h>
#include <stdio.h>
#include <string.h>
#include <wchar.h>
#include <windows.h>

#include "bluetooth.h"
#include "btstack_defines.h"
#include "btstack_memory.h"
#include "btstack_run_loop.h"
#include "btstack_run_loop_windows.h"
#include "gap.h"
#include "hci.h"
#include "hci_cmd.h"
#include "hci_dump.h"
#include "hci_event.h"
#include "hci_transport.h"
#include "l2cap.h"
#include "classic/a2dp_source.h"
#include "classic/avdtp.h"
#include "classic/avdtp_sink.h"
#include "classic/avdtp_source.h"
#include "classic/avdtp_util.h"
#include "classic/avrcp.h"
#include "classic/avrcp_controller.h"
#include "classic/avrcp_target.h"
#include "classic/btstack_link_key_db.h"
#include "classic/hfp_ag.h"
#include "classic/hid_host.h"
#include "classic/rfcomm.h"
#include "classic/btstack_link_key_db_memory.h"
#include "classic/sdp_client.h"
#include "classic/sdp_client_rfcomm.h"
#include "classic/sdp_server.h"
#include "classic/sdp_util.h"
#include "btstack_crypto.h"
#include "btstack_tlv.h"
#include "ble/att_db.h"
#include "ble/att_db_util.h"
#include "ble/att_server.h"
#include "ble/gatt_client.h"
#include "ble/le_device_db.h"
#include "ble/le_device_db_tlv.h"
#include "ble/sm.h"

/* ---- Implemented in Rust (btstack-sys/src/callbacks.rs) ---- */
void bluecaster_rs_on_wake(void);
void bluecaster_rs_on_timer(uint32_t id);
void bluecaster_rs_on_packet(uint8_t source, uint8_t packet_type, uint16_t channel, uint8_t *packet, uint16_t size);
int bluecaster_rs_transport_open(void);
int bluecaster_rs_transport_close(void);
int bluecaster_rs_transport_can_send(uint8_t packet_type);
int bluecaster_rs_transport_send(uint8_t packet_type, const uint8_t *packet, int size);
void bluecaster_rs_transport_set_sco_config(uint16_t voice_setting, uint8_t connections);
void bluecaster_rs_log_packet(uint8_t packet_type, uint8_t in, const uint8_t *packet, uint16_t len);
void bluecaster_rs_log_message(int level, const char *message);
int bluecaster_rs_lk_get(const uint8_t *addr, uint8_t *key, int *type);
void bluecaster_rs_lk_put(const uint8_t *addr, const uint8_t *key, int type);
void bluecaster_rs_lk_delete(const uint8_t *addr);
int bluecaster_rs_lk_at(int index, uint8_t *addr, uint8_t *key, int *type);
int bluecaster_rs_tlv_get(uint32_t tag, uint8_t *buffer, uint32_t size);
int bluecaster_rs_tlv_store(uint32_t tag, const uint8_t *data, uint32_t size);
void bluecaster_rs_tlv_delete(uint32_t tag);
uint16_t bluecaster_rs_att_read(uint16_t con, uint16_t handle, uint16_t offset, uint8_t *buffer, uint16_t buffer_size);
int bluecaster_rs_att_write(uint16_t con, uint16_t handle, uint16_t mode, uint16_t offset, const uint8_t *buffer,
                        uint16_t size);
void bluecaster_rs_att_can_send(uint16_t con);
void bluecaster_rs_le_disconnected(uint16_t con);
/* Called by the patched sm.c (btstack-sys build.rs, patched_sm) */
int bluecaster_rs_ctkd_enabled(void);
int bluecaster_rs_ctkd_wanted(const uint8_t *addr);
int bluecaster_ctkd_enabled(void) { return bluecaster_rs_ctkd_enabled(); }
int bluecaster_ctkd_wanted(const unsigned char *addr) { return bluecaster_rs_ctkd_wanted(addr); }
void bluecaster_rs_ctkd_note(const uint8_t *addr, int what);
void bluecaster_ctkd_note(const unsigned char *addr, int what) { bluecaster_rs_ctkd_note(addr, what); }
void bluecaster_rs_ctkd_from_le(int why, int addr_type, const uint8_t *addr, const uint8_t *key, int key_type);
void bluecaster_ctkd_from_le(int why, int addr_type, const unsigned char *addr, const unsigned char *key,
                             int key_type) {
    bluecaster_rs_ctkd_from_le(why, addr_type, addr, key, key_type);
}
/* Called by the patched le_device_db_tlv.c (patched_le_device_db) */
void bluecaster_rs_le_bond_evicted(int index, int addr_type, const uint8_t *addr);
int bluecaster_le_bond_in_use(int index);
void bluecaster_le_bond_evicted(int index, int addr_type, const unsigned char *addr);
/* Called by the patched att_server.c (patched_att_server) */
uint32_t bluecaster_ccc_tag(uint32_t slot);

static void cig_slot_release(uint8_t cig_id);

/* ======================================================================== */
/* Run loop: wake-up from other threads, timers                             */
/* ======================================================================== */

static btstack_context_callback_registration_t wake_registration;
static volatile LONG wake_pending;

static void wake_callback(void *context) {
    (void)context;
    /* Cleared first: a wake that comes while Rust runs queues a new callback */
    InterlockedExchange(&wake_pending, 0);
    bluecaster_rs_on_wake();
}

void bluecaster_runloop_init(void) {
    btstack_run_loop_init(btstack_run_loop_windows_get_instance());
    memset(&wake_registration, 0, sizeof(wake_registration));
    wake_registration.callback = &wake_callback;
    InterlockedExchange(&wake_pending, 0);
}

/* Any thread. One callback in the run loop's list at a time: the
 * registration is a list element and must not be added twice. */
void bluecaster_wake(void) {
    if (InterlockedExchange(&wake_pending, 1) == 0) {
        btstack_run_loop_execute_on_main_thread(&wake_registration);
    }
}

#define BLUECASTER_MAX_TIMERS 16
static btstack_timer_source_t timers[BLUECASTER_MAX_TIMERS];

static void timer_callback(btstack_timer_source_t *ts) {
    bluecaster_rs_on_timer((uint32_t)(uintptr_t)btstack_run_loop_get_timer_context(ts));
}

int bluecaster_timer_start(uint32_t id, uint32_t timeout_ms) {
    if (id >= BLUECASTER_MAX_TIMERS) return 0;
    btstack_timer_source_t *t = &timers[id];
    btstack_run_loop_remove_timer(t);
    btstack_run_loop_set_timer_handler(t, &timer_callback);
    btstack_run_loop_set_timer_context(t, (void *)(uintptr_t)id);
    btstack_run_loop_set_timer(t, timeout_ms);
    btstack_run_loop_add_timer(t);
    return 1;
}

void bluecaster_timer_stop(uint32_t id) {
    if (id >= BLUECASTER_MAX_TIMERS) return;
    btstack_run_loop_remove_timer(&timers[id]);
}

void bluecaster_timers_clear(void) {
    for (int i = 0; i < BLUECASTER_MAX_TIMERS; i++) btstack_run_loop_remove_timer(&timers[i]);
}

/* ======================================================================== */
/* HCI transport implemented in Rust                                        */
/* ======================================================================== */

static void (*transport_packet_handler)(uint8_t packet_type, uint8_t *packet, uint16_t size);
/* Incoming packets are copied here with BTstack's pre-buffer in front */
static uint8_t transport_rx_buffer[HCI_INCOMING_PRE_BUFFER_SIZE + HCI_ACL_PAYLOAD_SIZE + 4 + 260];

static void transport_init(const void *config) { (void)config; }
static int transport_open(void) { return bluecaster_rs_transport_open(); }
static int transport_close(void) { return bluecaster_rs_transport_close(); }
static void transport_register_packet_handler(void (*handler)(uint8_t, uint8_t *, uint16_t)) {
    transport_packet_handler = handler;
}
static int transport_can_send_packet_now(uint8_t packet_type) {
    return bluecaster_rs_transport_can_send(packet_type);
}
static int transport_send_packet(uint8_t packet_type, uint8_t *packet, int size) {
    /* LE Remove CIG: hci.c took the CIG out of its list when it sent this,
     * its storage may be used again */
    if (packet_type == HCI_COMMAND_DATA_PACKET && size >= 4 &&
        little_endian_read_16(packet, 0) == HCI_OPCODE_HCI_LE_REMOVE_CIG) {
        cig_slot_release(packet[3]);
    }
    return bluecaster_rs_transport_send(packet_type, packet, size);
}

static void transport_set_sco_config(uint16_t voice_setting, int num_connections) {
    bluecaster_rs_transport_set_sco_config(voice_setting, num_connections < 0 ? 0 : (uint8_t)num_connections);
}

/* Synchronous (send_packet returns when the packet is out, e.g. TCP): no
 * can_send_packet_now, HCI frees its buffer at once */
static const hci_transport_t transport_sync = {
    "bluecaster-sync", &transport_init, &transport_open, &transport_close,
    &transport_register_packet_handler, NULL, &transport_send_packet, NULL, NULL, NULL};
/* Asynchronous (USB): HCI waits for HCI_EVENT_TRANSPORT_PACKET_SENT */
static const hci_transport_t transport_async = {
    "bluecaster-async", &transport_init, &transport_open, &transport_close,
    &transport_register_packet_handler, &transport_can_send_packet_now, &transport_send_packet,
    NULL, NULL, NULL};
/* Asynchronous with SCO over USB isochronous: hci.c checks for the name
 * prefix "H2" (hci_have_usb_transport) and then sizes SCO payloads to 3 ms,
 * takes HCI_EVENT_SCO_CAN_SEND_NOW from the transport as its single send
 * token, and calls set_sco_config when SCO connections come and go. */
static const hci_transport_t transport_h2 = {
    "H2_BLUECASTER", &transport_init, &transport_open, &transport_close,
    &transport_register_packet_handler, &transport_can_send_packet_now, &transport_send_packet,
    NULL, NULL, &transport_set_sco_config};

/* kind: 0 synchronous, 1 asynchronous, 2 asynchronous H2 (USB with SCO) */
const hci_transport_t *bluecaster_transport_instance(int kind) {
    switch (kind) {
        case 2: return &transport_h2;
        case 1: return &transport_async;
        default: return &transport_sync;
    }
}

/* Returns 0 if the packet does not fit or no handler is registered */
int bluecaster_transport_deliver(uint8_t packet_type, const uint8_t *packet, uint16_t size) {
    if (transport_packet_handler == NULL) return 0;
    if ((size_t)size > sizeof(transport_rx_buffer) - HCI_INCOMING_PRE_BUFFER_SIZE) return 0;
    uint8_t *p = &transport_rx_buffer[HCI_INCOMING_PRE_BUFFER_SIZE];
    memcpy(p, packet, size);
    transport_packet_handler(packet_type, p, size);
    return 1;
}

void bluecaster_transport_packet_sent(void) {
    static uint8_t sent[] = {HCI_EVENT_TRANSPORT_PACKET_SENT, 0};
    if (transport_packet_handler) transport_packet_handler(HCI_EVENT_PACKET, sent, sizeof(sent));
}

/* A command the controller never answered: a Command Status of our own, as if
 * from the controller, gives hci.c its command slot back. hci.c keeps no
 * other state for a command once sent (handle_command_status_event only sets
 * num_cmd_packets for opcodes it does not track), so this is safe for
 * commands it does not wait on itself; the caller passes only its own link
 * reads. Its own buffer: the transport's may be in use further up. */
void bluecaster_hci_command_status_inject(uint16_t opcode, uint8_t status) {
    uint8_t event[6] = {HCI_EVENT_COMMAND_STATUS, 4, status, 1, (uint8_t)(opcode & 0xff), (uint8_t)(opcode >> 8)};
    if (transport_packet_handler) transport_packet_handler(HCI_EVENT_PACKET, event, sizeof(event));
}

static void handler_sco(uint8_t t, uint16_t c, uint8_t *p, uint16_t s);

/* kind as bluecaster_transport_instance */
void bluecaster_hci_init(int kind) {
    transport_packet_handler = NULL;
    hci_init(bluecaster_transport_instance(kind), NULL);
    /* hci.c calls the SCO handler without a NULL check when it emits
     * HCI_EVENT_SCO_CAN_SEND_NOW: registered before anything can ask */
    hci_register_sco_packet_handler(&handler_sco);
}

/* ======================================================================== */
/* HCI dump: packets and BTstack's log to Rust                              */
/* ======================================================================== */

static void dump_reset(void) {}

static void dump_log_packet(uint8_t packet_type, uint8_t in, uint8_t *packet, uint16_t len) {
    bluecaster_rs_log_packet(packet_type, in, packet, len);
}

static void dump_log_message(int log_level, const char *format, va_list argptr) {
    char buffer[512];
    vsnprintf(buffer, sizeof(buffer), format, argptr);
    bluecaster_rs_log_message(log_level, buffer);
}

static const hci_dump_t hci_dump_rust = {&dump_reset, &dump_log_packet, &dump_log_message};

void bluecaster_hci_dump_init(void) {
    hci_dump_init(&hci_dump_rust);
}

/* ======================================================================== */
/* Link key DB implemented in Rust                                          */
/* ======================================================================== */

static void lk_open(void) {}
static void lk_set_local_bd_addr(bd_addr_t bd_addr) { (void)bd_addr; }
static void lk_close(void) {}

static int lk_get_link_key(bd_addr_t bd_addr, link_key_t link_key, link_key_type_t *type) {
    int t = 0;
    if (!bluecaster_rs_lk_get(bd_addr, link_key, &t)) return 0;
    *type = (link_key_type_t)t;
    return 1;
}

static void lk_put_link_key(bd_addr_t bd_addr, link_key_t link_key, link_key_type_t type) {
    bluecaster_rs_lk_put(bd_addr, link_key, (int)type);
}

static void lk_delete_link_key(bd_addr_t bd_addr) { bluecaster_rs_lk_delete(bd_addr); }

/* The iterator is an index into the Rust store (deleting while iterating
 * is allowed and shifts the entries, as in v1's file DB) */
static int lk_iterator_init(btstack_link_key_iterator_t *it) {
    it->context = (void *)(intptr_t)0;
    return 1;
}

static int lk_iterator_get_next(btstack_link_key_iterator_t *it, bd_addr_t bd_addr, link_key_t link_key,
                                link_key_type_t *type) {
    int index = (int)(intptr_t)it->context;
    int t = 0;
    if (!bluecaster_rs_lk_at(index, bd_addr, link_key, &t)) return 0;
    it->context = (void *)(intptr_t)(index + 1);
    *type = (link_key_type_t)t;
    return 1;
}

static void lk_iterator_done(btstack_link_key_iterator_t *it) { it->context = NULL; }

static const btstack_link_key_db_t link_key_db_rust = {
    &lk_open, &lk_set_local_bd_addr, &lk_close, &lk_get_link_key, &lk_put_link_key,
    &lk_delete_link_key, &lk_iterator_init, &lk_iterator_get_next, &lk_iterator_done};

void bluecaster_set_link_key_db(int rust_store) {
    hci_set_link_key_db(rust_store ? &link_key_db_rust : btstack_link_key_db_memory_instance());
}

/* ======================================================================== */
/* Packet handlers                                                          */
/* ======================================================================== */

enum {
    SOURCE_HCI = 0,
    SOURCE_A2DP = 1,
    SOURCE_AVDTP_SINK = 2,
    SOURCE_AVRCP = 3,
    SOURCE_AVRCP_CONTROLLER = 4,
    SOURCE_AVRCP_TARGET = 5,
    SOURCE_SM = 6,
    SOURCE_GATT_CLIENT = 7,
    SOURCE_ATT_SERVER = 8,
    SOURCE_ISO = 9,
    SOURCE_HFP = 10,
    SOURCE_RFCOMM = 11,
    SOURCE_SDP_CLIENT = 12,
    SOURCE_SDP_SERVICES = 13,
    SOURCE_SCO = 14,
    SOURCE_HID = 15,
};

/* A keyboard being added that may ask for a passkey (bluecaster_hid_pairing_target) */
static int hid_pairing_armed;
static bd_addr_t hid_pairing_addr;

static void handler_hci(uint8_t t, uint16_t c, uint8_t *p, uint16_t s) {
    /* BTstack answers the IO Capability Request in hci_run, after this
     * handler: that device as a display (it shows the passkey to type),
     * every other one as before (no input, no output) */
    if (t == HCI_EVENT_PACKET && s >= 8 && p[0] == HCI_EVENT_IO_CAPABILITY_REQUEST) {
        bd_addr_t a;
        reverse_bd_addr(&p[2], a);
        int display = hid_pairing_armed && bd_addr_cmp(a, hid_pairing_addr) == 0;
        gap_ssp_set_io_capability(display ? SSP_IO_CAPABILITY_DISPLAY_ONLY : SSP_IO_CAPABILITY_NO_INPUT_NO_OUTPUT);
    }
    bluecaster_rs_on_packet(SOURCE_HCI, t, c, p, s);
}
static void handler_a2dp(uint8_t t, uint16_t c, uint8_t *p, uint16_t s) { bluecaster_rs_on_packet(SOURCE_A2DP, t, c, p, s); }
static void handler_avdtp_sink(uint8_t t, uint16_t c, uint8_t *p, uint16_t s) { bluecaster_rs_on_packet(SOURCE_AVDTP_SINK, t, c, p, s); }
static void handler_avrcp(uint8_t t, uint16_t c, uint8_t *p, uint16_t s) { bluecaster_rs_on_packet(SOURCE_AVRCP, t, c, p, s); }
static void handler_avrcp_ct(uint8_t t, uint16_t c, uint8_t *p, uint16_t s) { bluecaster_rs_on_packet(SOURCE_AVRCP_CONTROLLER, t, c, p, s); }
static void handler_avrcp_tg(uint8_t t, uint16_t c, uint8_t *p, uint16_t s) { bluecaster_rs_on_packet(SOURCE_AVRCP_TARGET, t, c, p, s); }
static void handler_sm(uint8_t t, uint16_t c, uint8_t *p, uint16_t s) { bluecaster_rs_on_packet(SOURCE_SM, t, c, p, s); }
static void handler_gatt_client(uint8_t t, uint16_t c, uint8_t *p, uint16_t s) { bluecaster_rs_on_packet(SOURCE_GATT_CLIENT, t, c, p, s); }
static void handler_att_server(uint8_t t, uint16_t c, uint8_t *p, uint16_t s) { bluecaster_rs_on_packet(SOURCE_ATT_SERVER, t, c, p, s); }
static void handler_hfp(uint8_t t, uint16_t c, uint8_t *p, uint16_t s) { bluecaster_rs_on_packet(SOURCE_HFP, t, c, p, s); }
static void handler_iso(uint8_t t, uint16_t c, uint8_t *p, uint16_t s) { bluecaster_rs_on_packet(SOURCE_ISO, t, c, p, s); }
static void handler_rfcomm(uint8_t t, uint16_t c, uint8_t *p, uint16_t s) { bluecaster_rs_on_packet(SOURCE_RFCOMM, t, c, p, s); }
static void handler_sdp_client(uint8_t t, uint16_t c, uint8_t *p, uint16_t s) { bluecaster_rs_on_packet(SOURCE_SDP_CLIENT, t, c, p, s); }
static void handler_sdp_services(uint8_t t, uint16_t c, uint8_t *p, uint16_t s) { bluecaster_rs_on_packet(SOURCE_SDP_SERVICES, t, c, p, s); }
/* SCO data (HCI_SCO_DATA_PACKET) and HCI_EVENT_SCO_CAN_SEND_NOW */
static void handler_sco(uint8_t t, uint16_t c, uint8_t *p, uint16_t s) { bluecaster_rs_on_packet(SOURCE_SCO, t, c, p, s); }

/* HID host events (HCI_EVENT_HID_META), reports included */
static void handler_hid(uint8_t t, uint16_t c, uint8_t *p, uint16_t s) { bluecaster_rs_on_packet(SOURCE_HID, t, c, p, s); }

static btstack_packet_callback_registration_t hci_event_registration;

void bluecaster_add_hci_event_handler(void) {
    memset(&hci_event_registration, 0, sizeof(hci_event_registration));
    hci_event_registration.callback = &handler_hci;
    hci_add_event_handler(&hci_event_registration);
}

void bluecaster_a2dp_source_init(void) {
    a2dp_source_init();
    a2dp_source_register_packet_handler(&handler_a2dp);
    /* No sink SEPs are registered, so the AVDTP sink callback only gets the
     * events AVDTP emits to both roles, SEP discovery among them */
    avdtp_register_sink_packet_handler(&handler_avdtp_sink);
}

void bluecaster_avrcp_init(void) {
    avrcp_init();
    avrcp_register_packet_handler(&handler_avrcp);
    avrcp_controller_init();
    avrcp_controller_register_packet_handler(&handler_avrcp_ct);
    avrcp_target_init();
    avrcp_target_register_packet_handler(&handler_avrcp_tg);
}

/* ======================================================================== */
/* HID host (experimental, developer mode): keyboards and mice              */
/* ======================================================================== */

/* The report descriptors of the connected devices, read over SDP */
static uint8_t hid_descriptor_storage[4096];

/* Output reports (the keyboard lights): hid_host_send_report keeps the
 * pointer until the interrupt channel can send, so each link has its own
 * copy here. A report sent again before the last one went out replaces it. */
#define BLUECASTER_HID_OUTPUT_LINKS 8
#define BLUECASTER_HID_OUTPUT_MAX 64
static struct {
    uint16_t cid;
    uint8_t data[BLUECASTER_HID_OUTPUT_MAX];
} hid_output[BLUECASTER_HID_OUTPUT_LINKS];

void bluecaster_hid_host_init(void) {
    hid_host_init(hid_descriptor_storage, sizeof(hid_descriptor_storage));
    hid_host_register_packet_handler(&handler_hid);
}

void bluecaster_hid_host_deinit(void) {
    hid_host_deinit();
    memset(hid_output, 0, sizeof(hid_output));
}

/* Classic: the device answered as a display in its pairing (see
 * handler_hci); NULL: none, and the IO capability is the usual one again */
void bluecaster_hid_pairing_target(const uint8_t *addr) {
    hid_pairing_armed = addr != NULL;
    if (addr) {
        memcpy(hid_pairing_addr, addr, sizeof(bd_addr_t));
    } else {
        gap_ssp_set_io_capability(SSP_IO_CAPABILITY_NO_INPUT_NO_OUTPUT);
    }
}

/* LE: the next pairing SM starts offers a display (1) or no input, no
 * output (0, the usual one). MITM stays as configured: a device that does
 * not ask for it pairs with Just Works either way. */
void bluecaster_sm_passkey_display(int display) {
    sm_set_io_capabilities(display ? IO_CAPABILITY_DISPLAY_ONLY : IO_CAPABILITY_NO_INPUT_NO_OUTPUT);
}

uint8_t bluecaster_hid_host_connect(const uint8_t *addr, uint16_t *out_cid) {
    bd_addr_t a;
    memcpy(a, addr, sizeof(a));
    return hid_host_connect(a, HID_PROTOCOL_MODE_REPORT, out_cid);
}

uint8_t bluecaster_hid_host_accept(uint16_t hid_cid) {
    return hid_host_accept_connection(hid_cid, HID_PROTOCOL_MODE_REPORT);
}

uint8_t bluecaster_hid_host_decline(uint16_t hid_cid) {
    return hid_host_decline_connection(hid_cid);
}

void bluecaster_hid_host_disconnect(uint16_t hid_cid) {
    hid_host_disconnect(hid_cid);
}

/* The device's report descriptor, copied to `out`; 0 when there is none or
 * it does not fit */
uint16_t bluecaster_hid_host_descriptor(uint16_t hid_cid, uint8_t *out, uint16_t max) {
    const uint8_t *data = hid_descriptor_storage_get_descriptor_data(hid_cid);
    uint16_t len = hid_descriptor_storage_get_descriptor_len(hid_cid);
    if (data == NULL || len == 0 || len > max) return 0;
    memcpy(out, data, len);
    return len;
}

/* An output report on the interrupt channel; report_id 0xFFFF: the device
 * has no report IDs */
uint8_t bluecaster_hid_host_send_output(uint16_t hid_cid, uint16_t report_id, const uint8_t *data, uint16_t len) {
    int slot = -1;
    if (hid_cid == 0 || len > BLUECASTER_HID_OUTPUT_MAX) return ERROR_CODE_INVALID_HCI_COMMAND_PARAMETERS;
    for (int i = 0; i < BLUECASTER_HID_OUTPUT_LINKS; i++) {
        if (hid_output[i].cid == hid_cid) { slot = i; break; }
        if (slot < 0 && hid_output[i].cid == 0) slot = i;
    }
    if (slot < 0) return ERROR_CODE_MEMORY_CAPACITY_EXCEEDED;
    hid_output[slot].cid = hid_cid;
    memcpy(hid_output[slot].data, data, len);
    return hid_host_send_report(hid_cid, report_id, hid_output[slot].data, len);
}

/* The link is closed: its output copy is free again */
void bluecaster_hid_host_output_closed(uint16_t hid_cid) {
    for (int i = 0; i < BLUECASTER_HID_OUTPUT_LINKS; i++) {
        if (hid_output[i].cid == hid_cid) hid_output[i].cid = 0;
    }
}

/* ======================================================================== */
/* HFP audio gateway: the service level connection (headphone battery), and */
/* with voice the audio connection (headset microphone, developer mode)     */
/* ======================================================================== */

#define BLUECASTER_HFP_RFCOMM_CHANNEL 4

static uint8_t sdp_hfp_ag_service[200];
static uint8_t hfp_codecs_cvsd[] = { HFP_CODEC_CVSD };
/* In the AG's preference order */
static uint8_t hfp_codecs_voice[] = { HFP_CODEC_LC3_SWB, HFP_CODEC_MSBC, HFP_CODEC_CVSD };
static hfp_ag_indicator_t hfp_ag_indicators[] = {
    /* index, name, min, max, status, mandatory, enabled, status changed */
    {1, "service",   0, 1, 1, 0, 0, 0},
    {2, "call",      0, 1, 0, 1, 1, 0},
    {3, "callsetup", 0, 3, 0, 1, 1, 0},
    {4, "battchg",   0, 5, 5, 0, 0, 0},
    {5, "signal",    0, 5, 5, 0, 1, 0},
    {6, "roam",      0, 1, 0, 0, 1, 0},
    {7, "callheld",  0, 2, 0, 1, 1, 0}
};
/* HF indicators: enhanced safety, battery level (the headphones report it
 * with AT+BIEV) */
static hfp_generic_status_indicator_t hfp_hf_indicators[] = { {1, 1}, {2, 1} };
static const char *hfp_call_hold_services[] = {"1", "2"};

/* The AG was set up with voice (hfp_codecs_voice) */
static int hfp_voice_on;

/* voice: codec negotiation with LC3-SWB, mSBC and CVSD, eSCO S4 (the
 * headset microphone); otherwise CVSD only, for the battery level */
void bluecaster_hfp_ag_init(int voice) {
    uint16_t features = (1 << HFP_AGSF_HF_INDICATORS) | (1 << HFP_AGSF_EXTENDED_ERROR_RESULT_CODES);
    uint8_t *codecs = hfp_codecs_cvsd;
    uint8_t codecs_nr = sizeof(hfp_codecs_cvsd);
    hfp_voice_on = voice;
    if (voice) {
        features |= (1 << HFP_AGSF_CODEC_NEGOTIATION) | (1 << HFP_AGSF_ESCO_S4);
        codecs = hfp_codecs_voice;
        codecs_nr = sizeof(hfp_codecs_voice);
    }
    hfp_ag_init(BLUECASTER_HFP_RFCOMM_CHANNEL);
    hfp_ag_init_supported_features(features);
    hfp_ag_init_codecs(codecs_nr, codecs);
    hfp_ag_init_ag_indicators(sizeof(hfp_ag_indicators) / sizeof(hfp_ag_indicators[0]), hfp_ag_indicators);
    hfp_ag_init_hf_indicators(sizeof(hfp_hf_indicators) / sizeof(hfp_hf_indicators[0]), hfp_hf_indicators);
    hfp_ag_init_call_hold_services(2, hfp_call_hold_services);
    /* Apple accessories (Sony, Bose...) report their battery with
     * AT+IPHONEACCEV once the AG answers AT+XAPL: as an iPhone does, with
     * the battery (2) and docking (4) features. Sony's WH-1000XM4 sends
     * nothing when the AG gives another name or no features. */
    hfp_ag_init_apple_identification("iPhone", 0x06);
    memset(sdp_hfp_ag_service, 0, sizeof(sdp_hfp_ag_service));
    /* hfp_ag_create_sdp_record_with_codecs sets the SDP wide band speech
     * (bit 5) and LC3-SWB (bit 8) bits only when the features it gets have
     * bit 7 set: HFP_HFSF_CODEC_NEGOTIATION, the HF's bit number, not the
     * AG's HFP_AGSF_CODEC_NEGOTIATION (bit 9). Bit 7 of the AG features
     * (enhanced call control) never reaches the record (only bits 0-4 are
     * copied), so it is added for the record only. */
    uint16_t sdp_features = features;
    if (voice) sdp_features |= (1 << HFP_HFSF_CODEC_NEGOTIATION);
    hfp_ag_create_sdp_record_with_codecs(sdp_hfp_ag_service, sdp_create_service_record_handle(),
                                         BLUECASTER_HFP_RFCOMM_CHANNEL, "BlueCaster", 0, sdp_features, codecs_nr,
                                         codecs);
    sdp_register_service(sdp_hfp_ag_service);
    hfp_ag_register_packet_handler(&handler_hfp);
}

uint8_t bluecaster_hfp_ag_connect(const uint8_t *addr) {
    bd_addr_t a;
    memcpy(a, addr, 6);
    return hfp_ag_establish_service_level_connection(a);
}

uint8_t bluecaster_hfp_ag_release(uint16_t acl_handle) {
    return hfp_ag_release_service_level_connection(acl_handle);
}

/* Audio connection on the service level connection of `acl_handle` (codec
 * negotiation first when both sides support it). The result comes as
 * HFP_SUBEVENT_AUDIO_CONNECTION_ESTABLISHED / _RELEASED (SOURCE_HFP). */
uint8_t bluecaster_hfp_ag_audio_connect(uint16_t acl_handle) {
    return hfp_ag_establish_audio_connection(acl_handle);
}

uint8_t bluecaster_hfp_ag_audio_release(uint16_t acl_handle) {
    return hfp_ag_release_audio_connection(acl_handle);
}

/* The voice codecs the headphones listed (AT+BAC: 1 CVSD, 2 mSBC, 3
 * LC3-SWB) on the service level connection of `acl_handle`, at most `max`
 * into `out`; CVSD alone without codec negotiation, which every HF has.
 * The number written; 0 without such a connection. */
uint8_t bluecaster_hfp_ag_remote_codecs(uint16_t acl_handle, uint8_t *out, uint8_t max) {
    hfp_connection_t *c = get_hfp_connection_context_for_acl_handle(acl_handle, HFP_ROLE_AG);
    if (c == NULL || max == 0) return 0;
    if ((c->remote_supported_features & (1u << HFP_HFSF_CODEC_NEGOTIATION)) == 0 || c->remote_codecs_nr == 0) {
        out[0] = HFP_CODEC_CVSD;
        return 1;
    }
    uint8_t n = 0;
    for (uint16_t i = 0; i < c->remote_codecs_nr && n < max; i++) out[n++] = c->remote_codecs[i];
    return n;
}

/* The voice codecs the AG offers from now on (HFP codec ids, in its
 * preference order, at most 3). BTstack suggests the first one the
 * headphones listed and CVSD when none is. Without voice (CVSD only, for
 * the battery) nothing changes. A codec negotiated before on the service
 * level connection of `acl_handle` is negotiated again on its next audio
 * connection (BTstack would reuse it). 0 = done, 1 = not applied. */
uint8_t bluecaster_hfp_ag_set_codecs(uint16_t acl_handle, const uint8_t *codecs, uint8_t codecs_nr) {
    if (!hfp_voice_on || codecs_nr == 0 || codecs_nr > sizeof(hfp_codecs_voice)) return 1;
    uint8_t list[sizeof(hfp_codecs_voice)];
    memcpy(list, codecs, codecs_nr);
    /* As BTstack does on each service level connection: mSBC needs eSCO
     * (the controller's features are known once HCI works) */
    if (hci_get_state() == HCI_STATE_WORKING) hfp_hf_drop_mSBC_if_eSCO_not_supported(list, &codecs_nr);
    hfp_ag_init_codecs(codecs_nr, list);
    hfp_connection_t *c = get_hfp_connection_context_for_acl_handle(acl_handle, HFP_ROLE_AG);
    if (c != NULL && c->codecs_state == HFP_CODECS_EXCHANGED && c->state == HFP_SERVICE_LEVEL_CONNECTION_ESTABLISHED &&
        (c->remote_supported_features & (1u << HFP_HFSF_CODEC_NEGOTIATION)) != 0 && c->remote_codecs_nr > 0) {
        c->codecs_state = HFP_CODECS_RECEIVED_LIST;
    }
    return 0;
}

/* +VGS: the headphones' speaker gain (0..15) on the voice link. BTstack
 * sends it only when it differs from the last gain either side gave. */
uint8_t bluecaster_hfp_ag_set_speaker_gain(uint16_t acl_handle, uint8_t gain) {
    return hfp_ag_set_speaker_gain(acl_handle, gain);
}

/* +VGM: the headphones' microphone gain (0..15) on the voice link; sent
 * only when it differs from the last gain either side gave, as +VGS. */
uint8_t bluecaster_hfp_ag_set_microphone_gain(uint16_t acl_handle, uint8_t gain) {
    return hfp_ag_set_microphone_gain(acl_handle, gain);
}

/* ---- SCO data ---- */

/* One SCO packet (header: handle, length; then the payload) in HCI's
 * outgoing buffer. 0 = sent; BTSTACK_ACL_BUFFERS_FULL when HCI cannot send
 * now (send only after HCI_EVENT_SCO_CAN_SEND_NOW). */
uint8_t bluecaster_sco_send(uint16_t sco_handle, const uint8_t *payload, uint16_t len) {
    if (len > 255) return ERROR_CODE_INVALID_HCI_COMMAND_PARAMETERS;
    if (hci_is_packet_buffer_reserved()) return BTSTACK_ACL_BUFFERS_FULL;
    hci_reserve_packet_buffer();
    uint8_t *p = hci_get_outgoing_packet_buffer();
    little_endian_store_16(p, 0, sco_handle);
    p[2] = (uint8_t)len;
    memcpy(&p[3], payload, len);
    return hci_send_sco_packet_buffer(3 + len);
}

void bluecaster_sco_request_can_send_now(uint16_t sco_handle) {
    hci_request_sco_can_send_now_event_for_con_handle(sco_handle);
}

/* The SCO packet length (header included) HCI wants for this connection */
uint16_t bluecaster_sco_packet_length(uint16_t sco_handle) {
    return hci_get_sco_packet_length_for_connection(sco_handle);
}

void bluecaster_hfp_ag_deinit(void) {
    hfp_ag_deinit();
    hfp_voice_on = 0;
}

/* ======================================================================== */
/* RFCOMM: the HFP audio gateway's, and client channels for IPC clients     */
/* ======================================================================== */

void bluecaster_rfcomm_init(void) {
    rfcomm_init();
}

static int sdp_rfcomm_wanted;

/* RFCOMM keeps its services in a static list: cleared here, or the next run
 * in this process finds the service freed by btstack_memory_deinit there */
void bluecaster_rfcomm_deinit(void) {
    rfcomm_deinit();
    sdp_rfcomm_wanted = 0;
}

/* Events of client channels (opened, closed, can send now) and their data
 * (RFCOMM_DATA_PACKET, channel = RFCOMM cid): SOURCE_RFCOMM */
uint8_t bluecaster_rfcomm_create_channel(const uint8_t *addr, uint8_t server_channel, uint16_t *out_cid) {
    bd_addr_t a;
    memcpy(a, addr, 6);
    return rfcomm_create_channel(&handler_rfcomm, a, server_channel, out_cid);
}

/* The RFCOMM server channel of a service: one query at a time (Rust queues
 * the others). It waits in the SDP client's queue, which HFP shares, until
 * the client is free; the results (SDP_EVENT_QUERY_RFCOMM_SERVICE, then
 * SDP_EVENT_QUERY_COMPLETE) go to SOURCE_SDP_CLIENT. */
static btstack_context_callback_registration_t sdp_rfcomm_request;
/* In the SDP client's request list. sdp_client_deinit keeps that list, so
 * this stays set across a restart of BTstack in this process until the
 * callback comes. */
static int sdp_rfcomm_listed;
/* Registered only to make the SDP client run its request list (see
 * bluecaster_sdp_query_rfcomm); its own turn does nothing */
static btstack_context_callback_registration_t sdp_rfcomm_kick;
static void sdp_rfcomm_kicked(void *context) {
    (void)context;
}
static bd_addr_t sdp_rfcomm_addr;
static uint8_t sdp_rfcomm_uuid[16];

static void sdp_rfcomm_start(void *context) {
    (void)context;
    sdp_rfcomm_listed = 0;
    if (!sdp_rfcomm_wanted) return;
    sdp_rfcomm_wanted = 0;
    uint8_t status;
    uint32_t short_uuid = big_endian_read_32(sdp_rfcomm_uuid, 0);
    /* A UUID of the Bluetooth base (SPP 0x1101...) is searched for in its
     * 16-bit form, which every SDP server matches */
    if (uuid_has_bluetooth_prefix(sdp_rfcomm_uuid) && short_uuid <= 0xFFFF) {
        status = sdp_client_query_rfcomm_channel_and_name_for_search_pattern(
            &handler_sdp_client, sdp_rfcomm_addr, sdp_service_search_pattern_for_uuid16((uint16_t)short_uuid));
    } else {
        status = sdp_client_query_rfcomm_channel_and_name_for_uuid128(&handler_sdp_client, sdp_rfcomm_addr,
                                                                      sdp_rfcomm_uuid);
    }
    if (status != ERROR_CODE_SUCCESS) {
        uint8_t event[3] = {SDP_EVENT_QUERY_COMPLETE, 1, status};
        handler_sdp_client(HCI_EVENT_PACKET, 0, event, sizeof(event));
    }
}

uint8_t bluecaster_sdp_query_rfcomm(const uint8_t *addr, const uint8_t *uuid128) {
    if (sdp_rfcomm_wanted) return ERROR_CODE_COMMAND_DISALLOWED;
    memcpy(sdp_rfcomm_addr, addr, 6);
    memcpy(sdp_rfcomm_uuid, uuid128, 16);
    sdp_rfcomm_wanted = 1;
    if (!sdp_rfcomm_listed) {
        /* Set first: the callback may come before this returns */
        sdp_rfcomm_listed = 1;
        sdp_rfcomm_request.callback = &sdp_rfcomm_start;
        uint8_t status = sdp_client_register_query_callback(&sdp_rfcomm_request);
        if (status != ERROR_CODE_SUCCESS) {
            sdp_rfcomm_listed = 0;
            sdp_rfcomm_wanted = 0;
            return status;
        }
    } else if (sdp_client_ready()) {
        /* Listed before a restart of BTstack, which left the client idle
         * with the list kept: nothing pops it until a registration does */
        sdp_rfcomm_kick.callback = &sdp_rfcomm_kicked;
        (void)sdp_client_register_query_callback(&sdp_rfcomm_kick);
    }
    return ERROR_CODE_SUCCESS;
}

/* The services a device lists (developer mode): every record with L2CAP in
 * its protocols, attributes ServiceClassIDList and ServiceName. Queued like
 * the RFCOMM query above; the results (SDP_EVENT_QUERY_ATTRIBUTE_VALUE per
 * byte, then SDP_EVENT_QUERY_COMPLETE) go to SOURCE_SDP_SERVICES. */
static btstack_context_callback_registration_t sdp_services_request;
static btstack_context_callback_registration_t sdp_services_kick;
static int sdp_services_wanted;
static int sdp_services_listed;
static bd_addr_t sdp_services_addr;
/* Own buffers: the SDP client reads them while the query runs */
static const uint8_t sdp_services_pattern[] = {0x35, 0x03, 0x19, 0x01, 0x00};
static const uint8_t sdp_services_attributes[] = {0x35, 0x06, 0x09, 0x00, 0x01, 0x09, 0x01, 0x00};

static void sdp_services_start(void *context) {
    (void)context;
    sdp_services_listed = 0;
    if (!sdp_services_wanted) return;
    sdp_services_wanted = 0;
    uint8_t status = sdp_client_query(&handler_sdp_services, sdp_services_addr, sdp_services_pattern,
                                      sdp_services_attributes);
    if (status != ERROR_CODE_SUCCESS) {
        uint8_t event[3] = {SDP_EVENT_QUERY_COMPLETE, 1, status};
        handler_sdp_services(HCI_EVENT_PACKET, 0, event, sizeof(event));
    }
}

uint8_t bluecaster_sdp_query_services(const uint8_t *addr) {
    if (sdp_services_wanted) return ERROR_CODE_COMMAND_DISALLOWED;
    memcpy(sdp_services_addr, addr, 6);
    sdp_services_wanted = 1;
    if (!sdp_services_listed) {
        sdp_services_listed = 1;
        sdp_services_request.callback = &sdp_services_start;
        uint8_t status = sdp_client_register_query_callback(&sdp_services_request);
        if (status != ERROR_CODE_SUCCESS) {
            sdp_services_listed = 0;
            sdp_services_wanted = 0;
            return status;
        }
    } else if (sdp_client_ready()) {
        sdp_services_kick.callback = &sdp_rfcomm_kicked;
        (void)sdp_client_register_query_callback(&sdp_services_kick);
    }
    return ERROR_CODE_SUCCESS;
}

/* sdp_client_deinit, and a services query asked for in this run is
 * forgotten (the next run may ask again) */
void bluecaster_sdp_client_deinit(void) {
    sdp_client_deinit();
    sdp_services_wanted = 0;
}

/* ======================================================================== */
/* GAP, SDP records, stream endpoints                                       */
/* ======================================================================== */

/* BTstack keeps the pointer: the name lives here (HCI's 248 bytes + NUL) */
static char local_name[249];

void bluecaster_gap_setup(uint32_t class_of_device, uint16_t page_timeout, const char *name) {
    /* SSP: Just Works (no display, no keyboard), General Bonding without MITM */
    gap_ssp_set_io_capability(SSP_IO_CAPABILITY_NO_INPUT_NO_OUTPUT);
    hid_pairing_armed = 0;
    gap_ssp_set_authentication_requirement(SSP_IO_AUTHREQ_MITM_PROTECTION_NOT_REQUIRED_GENERAL_BONDING);
    /* BTstack 1.8.2 changed these defaults (auto accept off, key size 16):
     * kept as they were, so that pairing and older sinks behave as before */
    gap_ssp_set_auto_accept(1);
    gap_set_required_encryption_key_size(7);
    strncpy(local_name, (name && name[0]) ? name : "BlueCaster", sizeof(local_name) - 1);
    local_name[sizeof(local_name) - 1] = 0;
    gap_set_local_name(local_name);
    gap_set_class_of_device(class_of_device);
    gap_set_allow_role_switch(true);
    gap_set_default_link_policy_settings(0x0005);  /* role switch + sniff */
    gap_set_page_timeout(page_timeout);
    /* BTstack's default is the standard inquiry (no RSSI, no extended
     * inquiry response): every device then needed a name request after the
     * inquiry. With EIR most answer with their name, as on other systems. */
    hci_set_inquiry_mode(INQUIRY_MODE_RSSI_AND_EIR);
}

static uint8_t sdp_a2dp_source_service[150];
static uint8_t sdp_avrcp_controller_service[200];
static uint8_t sdp_avrcp_target_service[200];

void bluecaster_sdp_register_records(uint16_t controller_features, uint16_t target_features) {
    memset(sdp_a2dp_source_service, 0, sizeof(sdp_a2dp_source_service));
    a2dp_source_create_sdp_record(sdp_a2dp_source_service, sdp_create_service_record_handle(),
                                  AVDTP_SOURCE_FEATURE_MASK_PLAYER, NULL, NULL);
    sdp_register_service(sdp_a2dp_source_service);

    memset(sdp_avrcp_controller_service, 0, sizeof(sdp_avrcp_controller_service));
    avrcp_controller_create_sdp_record(sdp_avrcp_controller_service, sdp_create_service_record_handle(),
                                       controller_features, NULL, NULL);
    sdp_register_service(sdp_avrcp_controller_service);

    memset(sdp_avrcp_target_service, 0, sizeof(sdp_avrcp_target_service));
    avrcp_target_create_sdp_record(sdp_avrcp_target_service, sdp_create_service_record_handle(),
                                   target_features, NULL, NULL);
    sdp_register_service(sdp_avrcp_target_service);
}

#define BLUECASTER_MAX_ENDPOINTS MAX_NR_AVDTP_STREAM_ENDPOINTS
/* aptX Adaptive's codec information is the longest: 40 bytes */
#define BLUECASTER_CODEC_INFO_MAX 40
static uint8_t endpoint_caps[BLUECASTER_MAX_ENDPOINTS][BLUECASTER_CODEC_INFO_MAX];
static uint8_t endpoint_config[BLUECASTER_MAX_ENDPOINTS][BLUECASTER_CODEC_INFO_MAX];

/* Returns the local SEID, 0 on failure */
uint8_t bluecaster_create_stream_endpoint(int slot, uint8_t media_codec_type, const uint8_t *caps, uint16_t caps_len,
                                      const uint8_t *config, uint16_t config_len) {
    if (slot < 0 || slot >= BLUECASTER_MAX_ENDPOINTS) return 0;
    if (caps_len > BLUECASTER_CODEC_INFO_MAX || config_len > BLUECASTER_CODEC_INFO_MAX) return 0;
    memcpy(endpoint_caps[slot], caps, caps_len);
    memcpy(endpoint_config[slot], config, config_len);
    avdtp_stream_endpoint_t *ep = a2dp_source_create_stream_endpoint(
        AVDTP_AUDIO, (avdtp_media_codec_type_t)media_codec_type, endpoint_caps[slot], caps_len,
        endpoint_config[slot], config_len);
    if (!ep) return 0;
    /* Delay Reporting: a sink that lists it sends its playout delay
       (A2DP_SUBEVENT_SIGNALING_DELAY_REPORT) once the stream is configured */
    avdtp_source_register_delay_reporting_category(avdtp_local_seid(ep));
    return avdtp_local_seid(ep);
}

/* ======================================================================== */
/* Stream configuration                                                     */
/* ======================================================================== */

uint8_t bluecaster_set_config_sbc(uint16_t a2dp_cid, uint8_t local_seid, uint8_t remote_seid,
                              uint16_t sampling_frequency, int channel_mode, int block_length, int subbands,
                              int allocation_method, uint8_t min_bitpool, uint8_t max_bitpool) {
    avdtp_configuration_sbc_t c;
    memset(&c, 0, sizeof(c));
    c.sampling_frequency = sampling_frequency;
    c.channel_mode = (avdtp_channel_mode_t)channel_mode;
    c.block_length = (avdtp_sbc_block_length_t)block_length;
    c.subbands = (avdtp_sbc_subbands_t)subbands;
    c.allocation_method = (avdtp_sbc_allocation_method_t)allocation_method;
    c.min_bitpool_value = min_bitpool;
    c.max_bitpool_value = max_bitpool;
    return a2dp_source_set_config_sbc(a2dp_cid, local_seid, remote_seid, &c);
}

uint8_t bluecaster_set_config_aac(uint16_t a2dp_cid, uint8_t local_seid, uint8_t remote_seid, int object_type,
                              uint32_t sampling_frequency, uint8_t channels, uint32_t bit_rate, uint8_t vbr) {
    avdtp_configuration_mpeg_aac_t c;
    memset(&c, 0, sizeof(c));
    c.object_type = (avdtp_aac_object_type_t)object_type;
    c.sampling_frequency = sampling_frequency;
    c.channels = channels;
    c.bit_rate = bit_rate;
    c.vbr = vbr;
    c.drc = false;
    return a2dp_source_set_config_mpeg_aac(a2dp_cid, local_seid, remote_seid, &c);
}

/* BTstack keeps the pointer of the codec information until the
 * configuration is done: a persistent copy */
static uint8_t other_config_info[BLUECASTER_CODEC_INFO_MAX];

uint8_t bluecaster_set_config_other(uint16_t a2dp_cid, uint8_t local_seid, uint8_t remote_seid, const uint8_t *info,
                                uint8_t info_len) {
    if (info_len > sizeof(other_config_info)) return ERROR_CODE_INVALID_HCI_COMMAND_PARAMETERS;
    /* BTstack asserts that the accepted configuration is as long as the
     * local SEP's configuration buffer. Some codecs have more than one
     * length (aptX LL: 8 bytes, 17 with a sink's extended capabilities;
     * issue #520): the buffer (endpoint_config, 40 bytes) takes this one's. */
    avdtp_stream_endpoint_t *ep = avdtp_get_stream_endpoint_for_seid(local_seid);
    if (ep == NULL || ep->media_codec_configuration_info == NULL) return ERROR_CODE_INVALID_HCI_COMMAND_PARAMETERS;
    ep->media_codec_configuration_len = info_len;
    memcpy(other_config_info, info, info_len);
    return a2dp_source_set_config_other(a2dp_cid, local_seid, remote_seid, other_config_info, info_len);
}

/* ======================================================================== */
/* Connection state queries                                                 */
/* ======================================================================== */

/* ACL link to addr: -1 none, 0 still paging, 1 open (handle set), 2 other,
 * 3 going down (our HCI Disconnect) */
int bluecaster_acl_state(const uint8_t *addr, uint16_t *con_handle) {
    hci_connection_t *c = hci_connection_for_bd_addr_and_type(addr, BD_ADDR_TYPE_ACL);
    if (c == NULL) return -1;
    if (c->state == SEND_CREATE_CONNECTION || c->state == SENT_CREATE_CONNECTION) return 0;
    if (c->state == OPEN) {
        if (con_handle) *con_handle = c->con_handle;
        return 1;
    }
    if (c->state == SEND_DISCONNECT || c->state == SENT_DISCONNECT) return 3;
    return 2;
}

static int avdtp_state(const avdtp_connection_t *c) {
    if (c == NULL) return -1;
    return (c->state == AVDTP_SIGNALING_CONNECTION_OPENED ||
            c->state == AVDTP_SIGNALING_CONNECTION_W4_L2CAP_DISCONNECTED) ? 1 : 2;
}

/* AVDTP connection: -1 none, 1 signaling channel open (a release follows a
 * disconnect), 2 not open yet (freed at once by a disconnect) */
int bluecaster_avdtp_state_for_addr(const uint8_t *addr, uint16_t *avdtp_cid) {
    bd_addr_t a;
    memcpy(a, addr, 6);
    avdtp_connection_t *c = avdtp_get_connection_for_bd_addr(a);
    if (c && avdtp_cid) *avdtp_cid = c->avdtp_cid;
    return avdtp_state(c);
}

int bluecaster_avdtp_state_for_cid(uint16_t avdtp_cid) {
    return avdtp_state(avdtp_get_connection_for_avdtp_cid(avdtp_cid));
}

/* ======================================================================== */
/* HCI commands                                                             */
/* ======================================================================== */

/* Not in BTstack's command table: HCI Set AFH Host Channel Classification
 * (10-byte map as 8 + 1 + 1 bytes) and HCI Read AFH Channel Map */
static const hci_cmd_t cmd_set_afh_host_channel_classification = {0x0C3F, "D11"};
static const hci_cmd_t cmd_read_afh_channel_map = {0x1406, "H"};

uint8_t bluecaster_cmd_set_afh_classification(const uint8_t *map) {
    return hci_send_cmd(&cmd_set_afh_host_channel_classification, map, map[8], map[9]);
}

uint8_t bluecaster_cmd_read_afh_channel_map(uint16_t con_handle) {
    return hci_send_cmd(&cmd_read_afh_channel_map, con_handle);
}

uint8_t bluecaster_cmd_read_rssi(uint16_t con_handle) {
    return hci_send_cmd(&hci_read_rssi, con_handle);
}

/* Link diagnostics: polled with RSSI (failed contacts, link quality) or read
   once per connection (remote version, supervision timeout) */
uint8_t bluecaster_cmd_read_failed_contact_counter(uint16_t con_handle) {
    return hci_send_cmd(&hci_read_failed_contact_counter, con_handle);
}

uint8_t bluecaster_cmd_read_link_quality(uint16_t con_handle) {
    return hci_send_cmd(&hci_read_link_quality, con_handle);
}

uint8_t bluecaster_cmd_read_remote_version(uint16_t con_handle) {
    return hci_send_cmd(&hci_read_remote_version_information, con_handle);
}

uint8_t bluecaster_cmd_read_link_supervision_timeout(uint16_t con_handle) {
    return hci_send_cmd(&hci_read_link_supervision_timeout, con_handle);
}

/* The ACL packet types BTstack asks for when it creates a connection (from
   the controller's buffer size and features, "may not be used" bits set) */
uint16_t bluecaster_usable_acl_packet_types(void) {
    return hci_usable_acl_packet_types();
}

uint8_t bluecaster_cmd_change_connection_packet_type(uint16_t con_handle, uint16_t packet_types) {
    return hci_send_cmd(&hci_change_connection_packet_type, con_handle, packet_types);
}

/* Role switch always; sniff as asked (the default set at init allows both) */
uint8_t bluecaster_cmd_write_link_policy(uint16_t con_handle, int allow_sniff) {
    uint16_t policy = LM_LINK_POLICY_ENABLE_ROLE_SWITCH | (allow_sniff ? LM_LINK_POLICY_ENABLE_SNIFF_MODE : 0);
    return hci_send_cmd(&hci_write_link_policy_settings, con_handle, policy);
}

/* What BTstack holds of a link; -1 when it has no such connection */
int bluecaster_acl_packets_in_controller(uint16_t con_handle) {
    hci_connection_t *c = hci_connection_for_handle(con_handle);
    return c ? (int)c->num_packets_sent : -1;
}

/* The remote address of a connection; 0 when there is none */
int bluecaster_connection_address(uint16_t con_handle, uint8_t *addr) {
    hci_connection_t *c = hci_connection_for_handle(con_handle);
    if (c == NULL) return 0;
    memcpy(addr, c->address, 6);
    return 1;
}

int bluecaster_acl_role(uint16_t con_handle) {
    hci_role_t r = gap_get_role(con_handle);
    return r == HCI_ROLE_INVALID ? -1 : (int)r;
}

int bluecaster_acl_encryption_key_size(uint16_t con_handle) {
    if (hci_connection_for_handle(con_handle) == NULL) return -1;
    return gap_encryption_key_size(con_handle);
}

/* BTstack's own remote features query: once per link, nothing is sent when it
   has them already; the answer comes as its HCI event */
void bluecaster_remote_features_query(uint16_t con_handle) {
    hci_remote_features_query(con_handle);
}

uint16_t bluecaster_max_acl_data_packet_length(void) {
    return hci_max_acl_data_packet_length();
}

uint8_t bluecaster_cmd_read_bd_addr(void) {
    return hci_send_cmd(&hci_read_bd_addr);
}

uint8_t bluecaster_cmd_read_local_version(void) {
    return hci_send_cmd(&hci_read_local_version_information);
}

uint8_t bluecaster_cmd_create_connection_cancel(const uint8_t *addr) {
    bd_addr_t a;
    memcpy(a, addr, 6);
    return hci_send_cmd(&hci_create_connection_cancel, a);
}

/* A remote name request to a connected device: GAP's queue leaves it unsent
   while the link is up (seen in the emulator), so it goes out directly. Page
   settings do not matter over a link: R1, no clock offset */
uint8_t bluecaster_cmd_remote_name_request(const uint8_t *addr) {
    bd_addr_t a;
    memcpy(a, addr, 6);
    return hci_send_cmd(&hci_remote_name_request, a, 1, 0, 0);
}

uint8_t bluecaster_cmd_remote_name_request_cancel(const uint8_t *addr) {
    bd_addr_t a;
    memcpy(a, addr, 6);
    return hci_send_cmd(&hci_remote_name_request_cancel, a);
}

int bluecaster_hci_get_state(void) {
    return (int)hci_get_state();
}

int bluecaster_hci_can_send_command_now(void) {
    return hci_can_send_command_packet_now() ? 1 : 0;
}

/* ======================================================================== */
/* LE (experimental LE Audio): set up only by Stack::le_init                */
/* ======================================================================== */

/* ---- Per-connection storage BTstack keeps pointers to ---- */

#define BLUECASTER_LE_SLOTS 16
#define BLUECASTER_ATT_VALUE_MAX 512

typedef struct {
    hci_con_handle_t con;                        /* HCI_CON_HANDLE_INVALID: free */
    gatt_client_notification_t listener;         /* all notifications / indications of con */
    int listening;
    uint8_t write_buffer[BLUECASTER_ATT_VALUE_MAX];  /* the value of a running (long) write */
    btstack_context_callback_registration_t send_registration;  /* ATT server can-send-now */
} le_slot_t;

static le_slot_t le_slots[BLUECASTER_LE_SLOTS];

static void le_slots_reset(void) {
    memset(le_slots, 0, sizeof(le_slots));
    for (int i = 0; i < BLUECASTER_LE_SLOTS; i++) le_slots[i].con = HCI_CON_HANDLE_INVALID;
}

static le_slot_t *le_slot_for(hci_con_handle_t con, int create) {
    if (con == HCI_CON_HANDLE_INVALID) return NULL;
    le_slot_t *free_slot = NULL;
    for (int i = 0; i < BLUECASTER_LE_SLOTS; i++) {
        if (le_slots[i].con == con) return &le_slots[i];
        if (free_slot == NULL && le_slots[i].con == HCI_CON_HANDLE_INVALID) free_slot = &le_slots[i];
    }
    if (!create || free_slot == NULL) return NULL;
    memset(free_slot, 0, sizeof(*free_slot));
    free_slot->con = con;
    return free_slot;
}

/* The connection is gone: stop listening and free the slot. Its ATT send
 * registration was in a list of the freed hci_connection_t only. */
static void le_slot_release(hci_con_handle_t con) {
    le_slot_t *slot = le_slot_for(con, 0);
    if (slot == NULL) return;
    if (slot->listening) gatt_client_stop_listening_for_characteristic_value_updates(&slot->listener);
    slot->listening = 0;
    slot->con = HCI_CON_HANDLE_INVALID;
}

/* ---- CIG storage (gap_cig_create keeps both pointers until the CIG is
 * removed) ---- */

#define BLUECASTER_CIG_SLOTS 4

typedef struct {
    le_audio_cig_t cig;
    le_audio_cig_params_t params;
    int in_use;      /* possibly in hci.c's CIG list */
    uint8_t cig_id;
} cig_slot_t;

static cig_slot_t cig_slots[BLUECASTER_CIG_SLOTS];

static void cig_slot_release(uint8_t cig_id) {
    for (int i = 0; i < BLUECASTER_CIG_SLOTS; i++)
        if (cig_slots[i].in_use && cig_slots[i].cig_id == cig_id) cig_slots[i].in_use = 0;
}

static void cig_slots_reset(void) {
    memset(cig_slots, 0, sizeof(cig_slots));
}

/* ---- Broadcast storage (Auracast source): per broadcast one extended +
 * periodic advertising set and one BIG, in slot `big_handle` (several
 * broadcasts at once, e.g. left and right as broadcasts of their own).
 * BTstack keeps pointers to the advertising data, the set and the BIG
 * parameters until the set is removed / the BIG terminated. ---- */

#define BLUECASTER_ADV_DATA_MAX 251
#define BLUECASTER_BROADCASTS 4
typedef struct {
    le_advertising_set_t adv_set;
    uint8_t adv_data[BLUECASTER_ADV_DATA_MAX];
    uint8_t pa_data[BLUECASTER_ADV_DATA_MAX];
    int adv_in_use;
    uint8_t adv_handle;
    le_audio_big_t big;
    le_audio_big_params_t big_params;
    int big_in_use;
} bc_slot_t;
static bc_slot_t bc_slots[BLUECASTER_BROADCASTS];

static void broadcast_reset(void) {
    for (int i = 0; i < BLUECASTER_BROADCASTS; i++) {
        bc_slots[i].adv_in_use = 0;
        bc_slots[i].big_in_use = 0;
    }
}

/* The slot of a BIG handle, NULL when out of range */
static bc_slot_t *bc_slot(uint8_t big_handle) {
    return big_handle < BLUECASTER_BROADCASTS ? &bc_slots[big_handle] : NULL;
}

/* ---- Housekeeping on HCI events (registered by bluecaster_le_init) ---- */

static btstack_packet_callback_registration_t le_hci_registration;

static void le_hci_handler(uint8_t packet_type, uint16_t channel, uint8_t *packet, uint16_t size) {
    (void)channel;
    if (packet_type != HCI_EVENT_PACKET || size < 2) return;
    switch (packet[0]) {
    case HCI_EVENT_DISCONNECTION_COMPLETE:
        if (size >= 6 && packet[2] == ERROR_CODE_SUCCESS) {
            hci_con_handle_t con = little_endian_read_16(packet, 3);
            le_slot_release(con);
            bluecaster_rs_le_disconnected(con);
        }
        break;
    case HCI_EVENT_META_GAP:
        /* A CIG that could not be created is not kept by hci.c */
        if (size >= 5 && packet[2] == GAP_SUBEVENT_CIG_CREATED && packet[3] != ERROR_CODE_SUCCESS)
            cig_slot_release(packet[4]);
        /* Nor a BIG that failed or ended */
        if (size >= 5 && packet[2] == GAP_SUBEVENT_BIG_CREATED && packet[3] != ERROR_CODE_SUCCESS) {
            bc_slot_t *s = bc_slot(packet[4]);
            if (s != NULL) s->big_in_use = 0;
        }
        if (size >= 4 && packet[2] == GAP_SUBEVENT_BIG_TERMINATED) {
            bc_slot_t *s = bc_slot(packet[3]);
            if (s != NULL) s->big_in_use = 0;
        }
        break;
    case BTSTACK_EVENT_STATE:
        /* HCI reset its state (power off): no CIG, BIG or advertising set is
         * kept */
        if (size >= 3 && packet[2] == HCI_STATE_OFF) {
            cig_slots_reset();
            broadcast_reset();
        }
        break;
    default:
        break;
    }
}

/* ---- Setup ---- */

static btstack_packet_callback_registration_t sm_event_registration;

/* The LE TLV is kept in Rust (btstack-sys TlvStore: the bonds file,
 * encrypted at rest, instead of btstack_tlv_windows' plain file) */
static int rs_tlv_get_tag(void *context, uint32_t tag, uint8_t *buffer, uint32_t buffer_size) {
    (void)context;
    return bluecaster_rs_tlv_get(tag, buffer, buffer_size);
}

static int rs_tlv_store_tag(void *context, uint32_t tag, const uint8_t *data, uint32_t data_size) {
    (void)context;
    return bluecaster_rs_tlv_store(tag, data, data_size);
}

static void rs_tlv_delete_tag(void *context, uint32_t tag) {
    (void)context;
    bluecaster_rs_tlv_delete(tag);
}

static const btstack_tlv_t le_tlv_rust = {&rs_tlv_get_tag, &rs_tlv_store_tag, &rs_tlv_delete_tag};

/* LE bonds in the Rust TLV store (le_device_db_tlv), SM with LE Secure
 * Connections, Just Works and bonding, GATT client, ISO. */
int bluecaster_le_init(void) {
    le_slots_reset();
    cig_slots_reset();
    broadcast_reset();
    btstack_tlv_set_instance(&le_tlv_rust, NULL);
    le_device_db_tlv_configure(&le_tlv_rust, NULL);

    /* sm_init sets up btstack_crypto only once per process: a later run's
     * hci_init has dropped its HCI event handler, so it never got its LE
     * Rand answers again and every SM computation (keys, pairing) waited
     * forever. Set up anew for each run (nothing of it is in use here). */
    btstack_crypto_deinit();
    sm_init();
    sm_set_io_capabilities(IO_CAPABILITY_NO_INPUT_NO_OUTPUT);
    sm_set_authentication_requirements(SM_AUTHREQ_SECURE_CONNECTION | SM_AUTHREQ_BONDING);
    /* BTstack 1.8.2 changed these defaults (Secure Connections Only, key
     * size 16 only): kept as they were (legacy pairing allowed, 7..16) */
    sm_set_secure_connections_only_mode(false);
    sm_set_encryption_key_size_range(7, 16);
    memset(&sm_event_registration, 0, sizeof(sm_event_registration));
    sm_event_registration.callback = &handler_sm;
    sm_add_event_handler(&sm_event_registration);

    gatt_client_init();

    /* Our LE links (BTstack's defaults: interval 10..30 ms, peripheral
     * latency 4, supervision timeout 720 ms): no peripheral latency, so
     * GATT discovery and the ASE control point answer every connection
     * event instead of every 5th, and a 5 s supervision timeout (as Android
     * uses). The short one dropped the ACL less than a second after the
     * first CIS came up on a Realtek adapter with three headphones: the
     * CIG's schedule leaves the ACL few events while it starts. */
    gap_set_connection_parameters(0x0060, 0x0030, 0x0008, 0x0018, 0, 500, 0, 0);

    memset(&le_hci_registration, 0, sizeof(le_hci_registration));
    le_hci_registration.callback = &le_hci_handler;
    hci_add_event_handler(&le_hci_registration);
    hci_register_iso_packet_handler(&handler_iso);
    return 1;
}

void bluecaster_le_deinit(int att_server_started) {
    for (int i = 0; i < BLUECASTER_LE_SLOTS; i++)
        if (le_slots[i].con != HCI_CON_HANDLE_INVALID) le_slot_release(le_slots[i].con);
    hci_remove_event_handler(&le_hci_registration);
    if (att_server_started) att_server_deinit();
    sm_deinit();
    btstack_tlv_set_instance(NULL, NULL);
    cig_slots_reset();
    broadcast_reset();
}

/* ---- GAP ---- */

uint8_t bluecaster_gap_connect(const uint8_t *addr, uint8_t addr_type) {
    bd_addr_t a;
    memcpy(a, addr, 6);
    return gap_connect(a, (bd_addr_type_t)addr_type);
}

/* Keyboards and mice (issue #413): the controller connects by itself to a
 * device of its accept list once that device advertises. One LE connection
 * attempt at a time: gap_connect is refused while this runs. */
uint8_t bluecaster_gap_auto_connection_start(uint8_t addr_type, const uint8_t *addr) {
    bd_addr_t a;
    memcpy(a, addr, 6);
    return gap_auto_connection_start((bd_addr_type_t)addr_type, a);
}

uint8_t bluecaster_gap_auto_connection_stop_all(void) {
    return gap_auto_connection_stop_all();
}

/* How much of the time a connection attempt listens for its device; the
 * other parameters as bluecaster_le_init sets them */
void bluecaster_gap_connection_scan(uint16_t interval, uint16_t window) {
    gap_set_connection_parameters(interval, window, 0x0008, 0x0018, 0, 500, 0, 0);
}

/* The controller has LE (only meaningful while LE is not hidden) */
int bluecaster_le_supported(void) {
    return hci_le_supported() ? 1 : 0;
}

int bluecaster_gap_connection_type(uint16_t con) {
    return (int)gap_get_connection_type(con);
}

void bluecaster_gap_delete_bonding(uint8_t addr_type, const uint8_t *addr) {
    bd_addr_t a;
    memcpy(a, addr, 6);
    gap_delete_bonding((bd_addr_type_t)addr_type, a);
}

/* HCI Disconnect for any handle (a CIS: gap_disconnect only knows ACL / SCO) */
uint8_t bluecaster_cmd_disconnect(uint16_t handle, uint8_t reason) {
    return hci_send_cmd(&hci_disconnect, handle, reason);
}

/* ---- TLV (the store le_device_db and att_server use) ---- */

/* Length of the tag's value (copied into buffer if not NULL), -1 without a
 * store, 0 if the tag is not there */
int bluecaster_tlv_get(uint32_t tag, uint8_t *buffer, uint32_t size) {
    const btstack_tlv_t *tlv = NULL;
    void *context = NULL;
    btstack_tlv_get_instance(&tlv, &context);
    if (tlv == NULL) return -1;
    return tlv->get_tag(context, tag, buffer, size);
}

int bluecaster_tlv_store(uint32_t tag, const uint8_t *data, uint32_t size) {
    const btstack_tlv_t *tlv = NULL;
    void *context = NULL;
    btstack_tlv_get_instance(&tlv, &context);
    if (tlv == NULL) return -1;
    return tlv->store_tag(context, tag, data, size);
}

void bluecaster_tlv_delete(uint32_t tag) {
    const btstack_tlv_t *tlv = NULL;
    void *context = NULL;
    btstack_tlv_get_instance(&tlv, &context);
    if (tlv != NULL) tlv->delete_tag(context, tag);
}

/* ---- LE bonds (le_device_db_tlv) and the CCC values of their clients ---- */

/* The bond belongs to an LE link that is up */
int bluecaster_le_bond_in_use(int index) {
    btstack_linked_list_iterator_t it;
    hci_connections_get_iterator(&it);
    while (btstack_linked_list_iterator_has_next(&it)) {
        hci_con_handle_t con = ((hci_connection_t *)btstack_linked_list_iterator_next(&it))->con_handle;
        /* (a classic link's index is not set before key derivation) */
        if (gap_get_connection_type(con) == GAP_CONNECTION_LE && sm_le_device_index(con) == index) return 1;
    }
    return 0;
}

/* le_device_db_add has no free entry and dropped the oldest bond: what was
 * kept beside it goes too, and the rest of BTstack and the application learn
 * of it as of any deleted bond (gap_delete_bonding) */
void bluecaster_le_bond_evicted(int index, int addr_type, const unsigned char *addr) {
    uint8_t buffer[20];
    bd_addr_t a;
    memcpy(a, addr, 6);
    bluecaster_rs_le_bond_evicted(index, addr_type, a);
    uint16_t size = hci_event_create_from_template_and_arguments(
        buffer, sizeof(buffer), &gap_subevent_bonding_deleted, addr_type, a, index, HCI_CON_HANDLE_INVALID);
    hci_emit_btstack_event(buffer, size, 1);
}

/* The TLV tag of a CCC value att_server keeps: 'BTC' + slot as in BTstack,
 * slots from 256 on 'AW1' + slot .. 'AW9' + slot (the next letter, 'BTD', are
 * the bonds) */
uint32_t bluecaster_ccc_tag(uint32_t slot) {
    uint32_t page = slot >> 8;
    if (page == 0) return ((uint32_t)'B' << 24) | ((uint32_t)'T' << 16) | ((uint32_t)'C' << 8) | slot;
    return ((uint32_t)'A' << 24) | ((uint32_t)'W' << 16) | (((uint32_t)'0' + page) << 8) | (slot & 0xFF);
}

uint32_t bluecaster_ccc_slots(void) { return NVN_NUM_GATT_SERVER_CCC; }

/* ---- GATT client (events of queries and listeners: SOURCE_GATT_CLIENT) ---- */

uint8_t bluecaster_gatt_discover_primary_services(uint16_t con) {
    return gatt_client_discover_primary_services(&handler_gatt_client, con);
}

static void make_service(gatt_client_service_t *s, uint16_t start, uint16_t end) {
    memset(s, 0, sizeof(*s));
    s->start_group_handle = start;
    s->end_group_handle = end;
}

uint8_t bluecaster_gatt_find_included_services(uint16_t con, uint16_t start, uint16_t end) {
    gatt_client_service_t s;
    make_service(&s, start, end);
    return gatt_client_find_included_services_for_service(&handler_gatt_client, con, &s);
}

uint8_t bluecaster_gatt_discover_characteristics(uint16_t con, uint16_t start, uint16_t end) {
    gatt_client_service_t s;
    make_service(&s, start, end);
    return gatt_client_discover_characteristics_for_service(&handler_gatt_client, con, &s);
}

uint8_t bluecaster_gatt_read_long_value(uint16_t con, uint16_t value_handle) {
    return gatt_client_read_long_value_of_characteristic_using_value_handle(&handler_gatt_client, con, value_handle);
}

/* Every attribute of this type in the range (ATT Read By Type), each as
 * GATT_EVENT_CHARACTERISTIC_VALUE_QUERY_RESULT with its handle: descriptors
 * too (the Report Reference of each HID report) */
uint8_t bluecaster_gatt_read_by_type(uint16_t con, uint16_t start, uint16_t end, uint16_t uuid16) {
    return gatt_client_read_value_of_characteristics_by_uuid16(&handler_gatt_client, con, start, end, uuid16);
}

/* BTstack keeps the value pointer until the write is done: a copy in the
 * connection's slot, made only when no other query runs (gatt_client allows
 * one per connection) */
uint8_t bluecaster_gatt_write_value(uint16_t con, uint16_t value_handle, const uint8_t *value, uint16_t len,
                                int long_write) {
    if (len > BLUECASTER_ATT_VALUE_MAX) return ERROR_CODE_INVALID_HCI_COMMAND_PARAMETERS;
    if (!gatt_client_is_ready(con)) return GATT_CLIENT_IN_WRONG_STATE;
    le_slot_t *slot = le_slot_for(con, 1);
    if (slot == NULL) return BTSTACK_MEMORY_ALLOC_FAILED;
    if (len > 0) memcpy(slot->write_buffer, value, len);
    return long_write
        ? gatt_client_write_long_value_of_characteristic(&handler_gatt_client, con, value_handle, len,
                                                         slot->write_buffer)
        : gatt_client_write_value_of_characteristic(&handler_gatt_client, con, value_handle, len,
                                                    slot->write_buffer);
}

/* Copied into the outgoing buffer at once */
uint8_t bluecaster_gatt_write_without_response(uint16_t con, uint16_t value_handle, const uint8_t *value, uint16_t len) {
    return gatt_client_write_value_of_characteristic_without_response(con, value_handle, len, (uint8_t *)value);
}

uint8_t bluecaster_gatt_write_client_configuration(uint16_t con, uint16_t start_handle, uint16_t value_handle,
                                               uint16_t end_handle, uint16_t properties, uint16_t uuid16,
                                               const uint8_t *uuid128, uint16_t configuration) {
    gatt_client_characteristic_t c;
    memset(&c, 0, sizeof(c));
    c.start_handle = start_handle;
    c.value_handle = value_handle;
    c.end_handle = end_handle;
    c.properties = properties;
    c.uuid16 = uuid16;
    if (uuid128) memcpy(c.uuid128, uuid128, 16);
    return gatt_client_write_client_characteristic_configuration(&handler_gatt_client, con, &c, configuration);
}

/* All notifications and indications of con (GATT_EVENT_NOTIFICATION /
 * _INDICATION to SOURCE_GATT_CLIENT); the listener is dropped when the
 * connection goes down */
int bluecaster_gatt_listen(uint16_t con) {
    le_slot_t *slot = le_slot_for(con, 1);
    if (slot == NULL) return 0;
    if (!slot->listening) {
        gatt_client_listen_for_characteristic_value_updates(&slot->listener, &handler_gatt_client, con, NULL);
        slot->listening = 1;
    }
    return 1;
}

void bluecaster_gatt_stop_listening(uint16_t con) {
    le_slot_t *slot = le_slot_for(con, 0);
    if (slot == NULL || !slot->listening) return;
    gatt_client_stop_listening_for_characteristic_value_updates(&slot->listener);
    slot->listening = 0;
}

/* ---- ATT server: database from att_db_util, callbacks to Rust ---- */

static uint16_t att_read_callback(hci_con_handle_t con, uint16_t handle, uint16_t offset, uint8_t *buffer,
                                  uint16_t buffer_size) {
    return bluecaster_rs_att_read(con, handle, offset, buffer, buffer_size);
}

static int att_write_callback(hci_con_handle_t con, uint16_t handle, uint16_t mode, uint16_t offset,
                              uint8_t *buffer, uint16_t buffer_size) {
    return bluecaster_rs_att_write(con, handle, mode, offset, buffer, buffer_size);
}

/* The database att_db_util built: att_server keeps the pointer, so
 * att_db_util must not be used to change it afterwards */
void bluecaster_att_server_init(void) {
    att_server_init(att_db_util_get_address(), &att_read_callback, &att_write_callback);
    att_server_register_packet_handler(&handler_att_server);
}

static void att_send_callback(void *context) {
    bluecaster_rs_att_can_send((uint16_t)(uintptr_t)context);
}

/* One request at a time per connection (the registration is a list item):
 * Rust asks again after the callback */
uint8_t bluecaster_att_server_request_to_send(uint16_t con, int indication) {
    le_slot_t *slot = le_slot_for(con, 1);
    if (slot == NULL) return BTSTACK_MEMORY_ALLOC_FAILED;
    slot->send_registration.callback = &att_send_callback;
    slot->send_registration.context = (void *)(uintptr_t)con;
    return indication ? att_server_request_to_send_indication(&slot->send_registration, con)
                      : att_server_request_to_send_notification(&slot->send_registration, con);
}

/* ---- CIG / CIS / ISO ---- */

/* The plain layout Rust fills (btstack-sys src/ffi.rs CisParams) */
typedef struct {
    uint8_t cis_id;
    uint8_t phy_c_to_p;
    uint8_t phy_p_to_c;
    uint8_t rtn_c_to_p;
    uint8_t rtn_p_to_c;
    uint16_t max_sdu_c_to_p;
    uint16_t max_sdu_p_to_c;
} bluecaster_cis_params_t;

uint8_t bluecaster_cig_create(uint8_t cig_id, uint32_t sdu_interval_c_to_p, uint32_t sdu_interval_p_to_c,
                          uint8_t worst_case_sca, uint8_t packing, uint8_t framing,
                          uint16_t max_transport_latency_c_to_p, uint16_t max_transport_latency_p_to_c,
                          uint8_t num_cis, const bluecaster_cis_params_t *cis) {
    if (num_cis == 0 || num_cis > MAX_NR_CIS) return ERROR_CODE_INVALID_HCI_COMMAND_PARAMETERS;
    cig_slot_t *slot = NULL;
    for (int i = 0; i < BLUECASTER_CIG_SLOTS; i++) {
        if (cig_slots[i].in_use && cig_slots[i].cig_id == cig_id) return ERROR_CODE_ACL_CONNECTION_ALREADY_EXISTS;
        if (slot == NULL && !cig_slots[i].in_use) slot = &cig_slots[i];
    }
    if (slot == NULL) return ERROR_CODE_MEMORY_CAPACITY_EXCEEDED;
    memset(slot, 0, sizeof(*slot));
    le_audio_cig_params_t *p = &slot->params;
    p->cig_id = cig_id;
    p->sdu_interval_c_to_p = sdu_interval_c_to_p;
    p->sdu_interval_p_to_c = sdu_interval_p_to_c;
    p->worst_case_sca = worst_case_sca;
    p->packing = packing;
    p->framing = framing;
    p->max_transport_latency_c_to_p = max_transport_latency_c_to_p;
    p->max_transport_latency_p_to_c = max_transport_latency_p_to_c;
    p->num_cis = num_cis;
    for (int i = 0; i < num_cis; i++) {
        p->cis_params[i].cis_id = cis[i].cis_id;
        p->cis_params[i].max_sdu_c_to_p = cis[i].max_sdu_c_to_p;
        p->cis_params[i].max_sdu_p_to_c = cis[i].max_sdu_p_to_c;
        p->cis_params[i].phy_c_to_p = cis[i].phy_c_to_p;
        p->cis_params[i].phy_p_to_c = cis[i].phy_p_to_c;
        p->cis_params[i].rtn_c_to_p = cis[i].rtn_c_to_p;
        p->cis_params[i].rtn_p_to_c = cis[i].rtn_p_to_c;
    }
    uint8_t status = gap_cig_create(&slot->cig, p);
    if (status == ERROR_CODE_SUCCESS) {
        slot->in_use = 1;
        slot->cig_id = cig_id;
    }
    return status;
}

/* One HCI ISO data packet with a complete SDU: handle | PB 0b10, no time
 * stamp, SDU header (sequence number, length). The caller keeps the
 * controller's queue (bt-host iso_send counts completions; BTstack does not
 * check ISO space). BTSTACK_ACL_BUFFERS_FULL while BTstack's one outgoing
 * packet buffer is taken: nothing sent, send it again after
 * HCI_EVENT_TRANSPORT_PACKET_SENT. */
uint8_t bluecaster_iso_send(uint16_t cis_handle, uint16_t sequence_number, const uint8_t *sdu, uint16_t len) {
    if ((uint32_t)len + 4u > HCI_ISO_PAYLOAD_SIZE) return ERROR_CODE_INVALID_HCI_COMMAND_PARAMETERS;
    if (hci_is_packet_buffer_reserved()) return BTSTACK_ACL_BUFFERS_FULL;
    hci_reserve_packet_buffer();
    uint8_t *buffer = hci_get_outgoing_packet_buffer();
    little_endian_store_16(buffer, 0, (uint16_t)((cis_handle & 0x0FFFu) | (2u << 12)));
    little_endian_store_16(buffer, 2, (uint16_t)(4u + len));
    little_endian_store_16(buffer, 4, sequence_number);
    little_endian_store_16(buffer, 6, len);
    if (len > 0) memcpy(buffer + 8, sdu, len);
    return hci_send_iso_packet_buffer((uint16_t)(8u + len));
}

/* ---- Broadcast (Auracast source) ---- */

/* Extended advertising (non-connectable, non-scannable) with `adv` and
 * periodic advertising with `pa` (the BASE), both started, in broadcast
 * slot `slot` (the BIG handle its BIG will get). Intervals in 0.625 ms
 * (extended) and 1.25 ms (periodic) units. */
uint8_t bluecaster_broadcast_advertise(uint8_t slot, uint8_t sid, uint16_t adv_interval_min,
                                   uint16_t adv_interval_max, uint16_t pa_interval_min, uint16_t pa_interval_max,
                                   const uint8_t *adv, uint16_t adv_len, const uint8_t *pa, uint16_t pa_len,
                                   uint8_t *out_handle) {
    bc_slot_t *s = bc_slot(slot);
    if (s == NULL) return ERROR_CODE_INVALID_HCI_COMMAND_PARAMETERS;
    if (s->adv_in_use) return ERROR_CODE_COMMAND_DISALLOWED;
    if (adv_len > BLUECASTER_ADV_DATA_MAX || pa_len > BLUECASTER_ADV_DATA_MAX)
        return ERROR_CODE_INVALID_HCI_COMMAND_PARAMETERS;
    memcpy(s->adv_data, adv, adv_len);
    memcpy(s->pa_data, pa, pa_len);
    le_extended_advertising_parameters_t params;
    memset(&params, 0, sizeof(params));
    params.advertising_event_properties = 0; /* non-connectable, non-scannable, undirected */
    params.primary_advertising_interval_min = adv_interval_min;
    params.primary_advertising_interval_max = adv_interval_max;
    params.primary_advertising_channel_map = 7;
    params.own_address_type = BD_ADDR_TYPE_LE_PUBLIC;
    params.advertising_tx_power = 127;    /* no preference */
    params.primary_advertising_phy = 1;   /* LE 1M */
    params.secondary_advertising_phy = 2; /* LE 2M */
    params.advertising_sid = sid;
    bd_addr_t local;
    gap_local_bd_addr(local);
    /* Slot 0 advertises from the public address; every other slot from a
     * static random address of its own (receivers such as Android list
     * broadcasters by address: two sets from one address show as one
     * broadcast whose name keeps changing). Derived from the public
     * address, so it stays the same from run to run. */
    int random = btstack_is_null_bd_addr(local) || slot > 0;
    if (random) params.own_address_type = BD_ADDR_TYPE_LE_RANDOM;
    uint8_t status = gap_extended_advertising_setup(&s->adv_set, &params, &s->adv_handle);
    if (status != ERROR_CODE_SUCCESS) return status;
    s->adv_in_use = 1;
    if (random) {
        bd_addr_t addr;
        if (btstack_is_null_bd_addr(local)) {
            static const bd_addr_t fallback = {0xC2, 0xA2, 0xD0, 0x00, 0x00, 0x01};
            memcpy(addr, fallback, 6);
        } else {
            memcpy(addr, local, 6);
        }
        /* Static random: top bits 0b11 (addr[0] is the most significant
         * octet), one per slot */
        addr[0] |= 0xC0;
        addr[5] = (uint8_t)(addr[5] ^ (0x5A + slot));
        gap_extended_advertising_set_random_address(s->adv_handle, addr);
    }
    gap_extended_advertising_set_adv_data(s->adv_handle, adv_len, s->adv_data);
    le_periodic_advertising_parameters_t pa_params;
    pa_params.periodic_advertising_interval_min = pa_interval_min;
    pa_params.periodic_advertising_interval_max = pa_interval_max;
    pa_params.periodic_advertising_properties = 0;
    gap_periodic_advertising_set_params(s->adv_handle, &pa_params);
    gap_periodic_advertising_set_data(s->adv_handle, pa_len, s->pa_data);
    gap_periodic_advertising_start(s->adv_handle, 0);
    status = gap_extended_advertising_start(s->adv_handle, 0, 0);
    *out_handle = s->adv_handle;
    return status;
}

/* Stops periodic and extended advertising of slot `slot` and removes its
 * set */
uint8_t bluecaster_broadcast_advertise_stop(uint8_t slot) {
    bc_slot_t *s = bc_slot(slot);
    if (s == NULL || !s->adv_in_use) return ERROR_CODE_SUCCESS;
    gap_periodic_advertising_stop(s->adv_handle);
    gap_extended_advertising_stop(s->adv_handle);
    uint8_t status = gap_extended_advertising_remove(s->adv_handle);
    s->adv_in_use = 0;
    return status;
}

/* LE Create BIG `big_handle` on the advertising set of slot `big_handle`
 * (GAP_SUBEVENT_BIG_CREATED follows, after BTstack set up the ISO data
 * paths). `code`: 16 bytes, or NULL for an unencrypted BIG. */
uint8_t bluecaster_big_create(uint8_t big_handle, uint8_t num_bis, uint32_t sdu_interval_us, uint16_t max_sdu,
                          uint16_t max_transport_latency_ms, uint8_t rtn, uint8_t phy, uint8_t framing,
                          const uint8_t *code) {
    bc_slot_t *s = bc_slot(big_handle);
    if (s == NULL) return ERROR_CODE_INVALID_HCI_COMMAND_PARAMETERS;
    if (!s->adv_in_use || s->big_in_use) return ERROR_CODE_COMMAND_DISALLOWED;
    if (num_bis == 0 || num_bis > MAX_NR_BIS) return ERROR_CODE_INVALID_HCI_COMMAND_PARAMETERS;
    le_audio_big_params_t *p = &s->big_params;
    memset(p, 0, sizeof(*p));
    p->big_handle = big_handle;
    p->advertising_handle = s->adv_handle;
    p->num_bis = num_bis;
    p->sdu_interval_us = sdu_interval_us;
    p->max_sdu = max_sdu;
    p->max_transport_latency_ms = max_transport_latency_ms;
    p->rtn = rtn;
    p->phy = phy;
    p->packing = 0; /* sequential */
    p->framing = framing;
    p->encryption = code != NULL;
    if (code != NULL) memcpy(p->broadcast_code, code, 16);
    uint8_t status = gap_big_create(&s->big, p);
    if (status == ERROR_CODE_SUCCESS) s->big_in_use = 1;
    return status;
}

/* LE Terminate BIG (GAP_SUBEVENT_BIG_TERMINATED follows) */
uint8_t bluecaster_big_terminate(uint8_t big_handle) {
    bc_slot_t *s = bc_slot(big_handle);
    if (s == NULL || !s->big_in_use) return ERROR_CODE_COMMAND_DISALLOWED;
    return gap_big_terminate(big_handle);
}
