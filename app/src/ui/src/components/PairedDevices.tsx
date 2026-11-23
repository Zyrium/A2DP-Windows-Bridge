// What Windows paired through an adapter about to go to WinUSB (issue #86:
// a built-in adapter takes the keyboard and mouse with it). The setup asks
// first when there are any; the Adapters screen's dialog names them. A list
// that cannot be read asks nothing. Every device is named (issue #86: not
// "and N more").

import { For, Show } from "solid-js";
import type { AppContext } from "../app/context";
import { t } from "../i18n";
import { titleWhenCut } from "./controls";
import { Icon } from "./Icon";
import type { AdapterInfo, WindowsPairedDevice } from "../ipc/types";

/** The devices Windows paired through the adapter; none when it is on
 *  WinUSB already or the list cannot be read */
export function windowsDevices(app: AppContext, a: AdapterInfo): Promise<WindowsPairedDevice[]> {
  if (a.driver === "winusb") return Promise.resolve([]);
  return app.core
    .request("adapter.windows_devices", { instance_id: a.instance_id })
    .then((list) => list ?? [])
    .catch(() => []);
}

/** The warning (issue #86): the devices first, one per line with what kind
 *  they are (headphones or another Bluetooth device; Windows' name, else
 *  the address), in a box in the warning colour with what happens to them;
 *  then what keeps them */
export function PairedDevicesNote(props: { devices: readonly WindowsPairedDevice[] }) {
  return (
    <Show when={props.devices.length > 0}>
      <div class="paired-warn">
        <ul class="paired-list">
          <For each={props.devices}>
            {(d) => (
              <li>
                <Icon name={d.audio ? "headphones" : "bluetooth"} />
                <strong class="clamp-2" ref={titleWhenCut(() => d.name || d.address)}>
                  {d.name || d.address}
                </strong>
              </li>
            )}
          </For>
        </ul>
        <p>{t("ui.adapter.paired_text")}</p>
      </div>
      <ul class="hint paired-hints">
        <li>{t("ui.adapter.paired_receiver")}</li>
        <li>{t("ui.adapter.paired_keep")}</li>
      </ul>
    </Show>
  );
}
