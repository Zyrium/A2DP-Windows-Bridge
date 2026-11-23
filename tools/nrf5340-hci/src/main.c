/*
 * Copyright (c) 2018 Intel Corporation
 *
 * SPDX-License-Identifier: Apache-2.0
 */

#include <zephyr/kernel.h>
#include <zephyr/sys/printk.h>
#include <zephyr/usb/usbd.h>

#include <sample_usbd.h>

int main(void)
{
	struct usbd_context *sample_usbd;
	int ret;

	sample_usbd = sample_usbd_setup_device(NULL);
	if (sample_usbd == NULL) {
		printk("Failed to initialize USB device");
		return -ENODEV;
	}

	/*
	 * Wireless Controller / RF / Bluetooth (E0/01/01) on the device, as a
	 * Bluetooth dongle has: with the sample's 00/00/00 and two interfaces
	 * Windows loads its composite driver and puts the HCI on a child node
	 * (MI_00), which BlueCaster does not list as an adapter.
	 */
	usbd_device_set_code_triple(sample_usbd, USBD_SPEED_FS, 0xE0, 0x01, 0x01);

	ret = usbd_init(sample_usbd);
	if (ret != 0) {
		printk("Failed to initialize device support");
		return -ENODEV;
	}

	ret = usbd_enable(sample_usbd);
	if (ret != 0) {
		printk("Failed to enable USB");
		return 0;
	}

	printk("Bluetooth over USB sample\n");
	return 0;
}
