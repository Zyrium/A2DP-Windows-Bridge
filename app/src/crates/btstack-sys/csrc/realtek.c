/*
 * realtek.c - BTstack's Realtek chipset driver, plus a read of its USB
 * firmware table
 *
 * fw_patch_table_usb is static in btstack_chipset_realtek.c and BTstack only
 * exports its VID:PID pairs. The app also needs each PID's firmware name to
 * tell the chip family (0x4853 and 0xB85B are both RTL8852BU), so this file
 * compiles the driver itself and reads the table from the same unit.
 *
 * SPDX-License-Identifier: Apache-2.0 OR MIT
 */

#include <stdio.h>
#include <stdlib.h>

/* BTstack's driver closes the config file with fclose() also when there was
 * none to open (issue #86: Realtek's driver firmware comes without one, and
 * a folder may hold only the firmware). The MSVC CRT treats fclose(NULL) as
 * an invalid parameter and ends the process at once: no crash report, the
 * last log lines lost. Its file I/O goes through this instead. */
static int bluecaster_fclose(FILE *file) {
    return file != NULL ? fclose(file) : 0;
}
#define fclose bluecaster_fclose

/* build.rs writes it to OUT_DIR: BTstack's file with a "RTBTCore" firmware
 * accepted on chips without a security key (patched_realtek) */
#include "btstack_chipset_realtek_patched.c"

#undef fclose

/* The firmware name (no extension, e.g. "rtl8852bu_fw") of entry `index` of
 * btstack_chipset_realtek_get_vendor_product_id's table */
const char *bluecaster_realtek_usb_fw_name(uint16_t index) {
    if (index >= btstack_chipset_realtek_get_num_usb_controllers()) return NULL;
    return fw_patch_table_usb[index].patch_name;
}

/* BTstack's driver opens "${folder}/${name}" with no extension, while the
 * files carry .bin (linux-firmware, the app's download). Its init builds the
 * paths again at each power on, so this init runs it and then appends .bin
 * where that file is there; an extension-less file alone is still taken. */
static void bluecaster_append_bin(char *path, size_t size) {
    size_t len = strlen(path);
    if (len + 5 > size) return;
    memcpy(&path[len], ".bin", 5);
    FILE *f = fopen(path, "rb");
    if (f != NULL) {
        fclose(f);
        return;
    }
    path[len] = '\0';
}

/* A firmware file of another name and folder (Realtek's Windows driver's,
 * issue #427); NULL: the folder's. The config file stays the folder's. */
static const char *bluecaster_firmware_override;

void bluecaster_realtek_set_firmware_file(const char *path) {
    bluecaster_firmware_override = path;
}

static void bluecaster_realtek_init(const void *config) {
    chipset_init(config);
    /* The USB lookup by product ID: it only names the files here, loading
     * them later (the UART lookup has loaded them already) */
    if (product_id != 0 && patch_usb != NULL) {
        if (bluecaster_firmware_override != NULL && strlen(bluecaster_firmware_override) < sizeof(firmware_file)) {
            strcpy(firmware_file, bluecaster_firmware_override);
        } else {
            bluecaster_append_bin(firmware_file, sizeof(firmware_file));
        }
        bluecaster_append_bin(config_file, sizeof(config_file));
    }
}

/* For tests: the length of the patch BTstack's download builds for USB
 * product `pid` (ROM version `rom`) from `firmware` and `config`, either of
 * which may be missing; 0 if the driver refuses them. Only the file reading
 * runs, no HCI. */
uint32_t bluecaster_realtek_patch_length(uint16_t pid, const char *firmware, const char *config, uint8_t rom) {
    const patch_info_usb *entry = NULL;
    for (uint16_t i = 0; i < btstack_chipset_realtek_get_num_usb_controllers(); i++) {
        if (fw_patch_table_usb[i].prod_id == pid) {
            entry = &fw_patch_table_usb[i];
            break;
        }
    }
    if (entry == NULL || patch_buffer != NULL) return 0;
    realtek_interface = REALTEK_INTERFACE_USB;
    rtb_cfg.lmp_subversion = entry->lmp_sub;
    rtb_cfg.chip_type = entry->chip_type;
    rom_version = rom;
    firmware_size = 0;
    config_size = 0;
    if (!load_firmware_and_config(firmware, config)) return 0;
    uint32_t length = firmware_size + config_size;
    free(patch_buffer);
    patch_buffer = NULL;
    return length;
}

static btstack_chipset_t bluecaster_realtek;

/* BTstack's Realtek chipset with the init above */
const btstack_chipset_t *bluecaster_realtek_instance(void) {
    bluecaster_realtek = btstack_chipset_realtek;
    bluecaster_realtek.init = bluecaster_realtek_init;
    return &bluecaster_realtek;
}
