# SPDX-License-Identifier: Apache-2.0
# nRF5340 as a USB HCI controller for BlueCaster's Auracast (four BIGs).
# Zephyr's hci_usb sample (nRF Connect SDK v3.4.1) with the USB HCI class
# replaced by src/bt_hci_iso.c, which passes HCI ISO data on the bulk pipe.

cmake_minimum_required(VERSION 3.20.0)
find_package(Zephyr REQUIRED HINTS $ENV{ZEPHYR_BASE})
project(bluecaster_nrf5340_hci)

include(${ZEPHYR_BASE}/samples/subsys/usb/common/common.cmake)
target_sources(app PRIVATE src/main.c src/bt_hci_iso.c)
