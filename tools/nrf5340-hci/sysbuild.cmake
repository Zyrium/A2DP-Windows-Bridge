# SPDX-License-Identifier: Apache-2.0
# The network core image (hci_ipc): Nordic's SoftDevice Controller set up
# for four BIGs (sysbuild/hci_ipc_sdc.conf). The open Zephyr LL
# (sysbuild/hci_ipc_ll_sw.conf with the snippet bt-ll-sw-split) ran two
# BIGs and never completed a third.
set(hci_ipc_CONF_FILE ${CMAKE_CURRENT_LIST_DIR}/sysbuild/hci_ipc_sdc.conf CACHE STRING "" FORCE)
