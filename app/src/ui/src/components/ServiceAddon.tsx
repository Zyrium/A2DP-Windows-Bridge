// The service add-on (issue #413): an optional step for keyboards and mice
// that makes them work on the sign-in screen and in administrator windows
// too. Without it the feature works as before, in ordinary windows only.
// Adding, updating and removing run the set-up tool as administrator
// (Windows' own prompt; src-tauri/src/service.rs). Adding the service does
// not turn keyboards and mice on: HidEnableDialog still does that.
//
// The window reaches the Developer screen only while a core serves it, so
// "stopped" and "needs an update" show on the connection screen instead
// (ServiceConnectionNotice), where no developer mode is needed.

import { createEffect, createSignal, Match, on, onMount, Show, Switch } from "solid-js";
import { useApp } from "../app/context";
import { t } from "../i18n";
import type { ServiceAction, ServiceSetupResult, ServiceState } from "../ipc/transport";
import { LINKS } from "../lib/links";
import { Button, Check, Dialog } from "./controls";

/** The message for a set-up that did not end well, or null */
export function setupError(r: ServiceSetupResult): string | null {
  switch (r.outcome) {
    case "done":
      return null;
    case "declined":
      return t("error.admin_declined");
    case "failed":
      return t("ui.service.failed", r.code);
    case "not_started":
      return t("ui.service.not_started");
  }
}

/** Runs one set-up at a time; the state and the last error as signals */
function useServiceSetup(initial: ServiceState | null = null) {
  const app = useApp();
  const [state, setState] = createSignal<ServiceState | null>(initial);
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  const read = async () => {
    try {
      setState(await app.core.transport.serviceState());
    } catch {
      setState(null);
    }
  };
  const run = async (action: ServiceAction, removeData = false): Promise<boolean> => {
    setBusy(true);
    setError(null);
    try {
      const r = await app.core.transport.serviceSetup(action, removeData);
      setState(r.service);
      setError(setupError(r));
      return r.outcome === "done";
    } catch {
      setError(t("ui.service.not_started"));
      return false;
    } finally {
      setBusy(false);
    }
  };
  return { state, busy, error, read, run };
}

/** Asks before adding: what the service means for this PC */
export function ServiceAddDialog(props: { open: boolean; busy: boolean; onConfirm: () => void; onCancel: () => void }) {
  return (
    <Dialog
      open={props.open}
      title={t("ui.service.add_title")}
      confirm={t("ui.service.add")}
      confirmIcon="shield"
      cancel={t("modal.cancel")}
      busy={props.busy}
      onConfirm={() => props.onConfirm()}
      onCancel={() => props.onCancel()}
    >
      <p>{t("ui.service.add_all_users")}</p>
      <p>{t("ui.service.add_headphones")}</p>
    </Dialog>
  );
}

/** Asks before removing; deleting the service's data is off unless chosen */
export function ServiceRemoveDialog(props: {
  open: boolean;
  busy: boolean;
  onConfirm: (removeData: boolean) => void;
  onCancel: () => void;
}) {
  const [data, setData] = createSignal(false);
  // Each opening starts with the data kept
  createEffect(
    on(
      () => props.open,
      (open) => {
        if (open) setData(false);
      },
    ),
  );
  return (
    <Dialog
      open={props.open}
      title={t("ui.service.remove_title")}
      warn
      confirm={t("ui.service.remove")}
      confirmIcon="shield"
      cancel={t("modal.cancel")}
      busy={props.busy}
      onConfirm={() => props.onConfirm(data())}
      onCancel={() => props.onCancel()}
    >
      <p>{t("ui.service.remove_text")}</p>
      <Check label={t("ui.service.remove_data")} checked={data()} onChange={setData} />
    </Dialog>
  );
}

const STATE_TEXT: Record<ServiceState["state"], string> = {
  not_added: "ui.service.state_not_added",
  running: "ui.service.state_running",
  stopped: "ui.service.state_stopped",
  needs_update: "ui.service.state_needs_update",
  app_older: "ui.service.state_app_older",
};

/** Developer > keyboards and mice: the optional service, by what it gives */
export function ServiceRow() {
  const s = useServiceSetup();
  const [adding, setAdding] = createSignal(false);
  const [removing, setRemoving] = createSignal(false);
  onMount(() => void s.read());
  // The dialog closes however the set-up ended; the row shows an error
  const add = async () => {
    await s.run("install");
    setAdding(false);
  };
  const remove = async (data: boolean) => {
    await s.run("remove", data);
    setRemoving(false);
  };
  return (
    <Show when={s.state()}>
      {(st) => (
        <div class="row" data-testid="service-row">
          <span class="row-text">
            <span class="row-label">{t("ui.service.row")}</span>
            <span class="hint">{t(STATE_TEXT[st().state])}</span>
            <Show when={!adding() && !removing() && s.error()}>
              {(e) => (
                <span class="field-error" role="alert">
                  {e()}
                </span>
              )}
            </Show>
          </span>
          <span class="inline">
            <Switch>
              <Match when={st().state === "not_added"}>
                <Button icon="shield" disabled={s.busy()} onClick={() => setAdding(true)}>
                  {t("ui.service.add")}
                </Button>
              </Match>
              <Match when={st().state === "needs_update"}>
                <Button icon="shield" busy={s.busy()} onClick={() => void s.run("update")}>
                  {t("ui.service.update")}
                </Button>
              </Match>
              {/* Interactive users may not start a service: the update
                  starts it, with an administrator's approval */}
              <Match when={st().state === "stopped"}>
                <Button icon="shield" busy={s.busy()} onClick={() => void s.run("update")}>
                  {t("ui.service.start")}
                </Button>
              </Match>
            </Switch>
            <Show when={st().state !== "not_added" && st().state !== "app_older"}>
              <Button kind="subtle" disabled={s.busy()} onClick={() => setRemoving(true)}>
                {t("ui.service.remove")}
              </Button>
            </Show>
          </span>
          <ServiceAddDialog open={adding()} busy={s.busy()} onConfirm={() => void add()} onCancel={() => setAdding(false)} />
          <ServiceRemoveDialog
            open={removing()}
            busy={s.busy()}
            onConfirm={(d) => void remove(d)}
            onCancel={() => setRemoving(false)}
          />
        </div>
      )}
    </Show>
  );
}

/** The connection screen while the service is newer than the app: only
 *  the app's update helps (setting the service up from here would take it
 *  back to this older version for everyone) */
export function ServiceNewerNotice(props: { version: string; app: string }) {
  const app = useApp();
  return (
    <>
      <h1>{t("ui.service.newer_title")}</h1>
      <p>{t("ui.service.newer_body")}</p>
      <Show when={props.version && props.app}>
        <p class="muted mono">{t("ui.service.versions", props.app, props.version)}</p>
      </Show>
      <div class="inline conn-actions">
        <Button kind="strong" icon="open" onClick={() => void app.core.transport.openUrl(LINKS.releases)}>
          {t("ui.settings.open_download")}
        </Button>
      </div>
    </>
  );
}

/** The connection screen while the service is older or stopped: no core
 *  serves this window until the service is updated or started */
export function ServiceConnectionNotice(props: { stopped: boolean; version?: string; app?: string }) {
  const s = useServiceSetup();
  const [removing, setRemoving] = createSignal(false);
  const remove = async (data: boolean) => {
    await s.run("remove", data);
    setRemoving(false);
  };
  return (
    <>
      <Show
        when={props.stopped}
        fallback={
          <>
            <h1>{t("ui.service.outdated_title")}</h1>
            <p>{t("ui.service.outdated_body")}</p>
            <Show when={props.version && props.app}>
              <p class="muted mono">{t("ui.service.versions", props.app ?? "", props.version ?? "")}</p>
            </Show>
          </>
        }
      >
        <h1>{t("ui.service.stopped_title")}</h1>
        <p>{t("ui.service.stopped_body")}</p>
      </Show>
      <div class="inline conn-actions">
        <Button kind="strong" icon="shield" busy={s.busy()} onClick={() => void s.run("update")}>
          {props.stopped ? t("ui.service.start") : t("ui.service.update")}
        </Button>
        <Button kind="subtle" disabled={s.busy()} onClick={() => setRemoving(true)}>
          {t("ui.service.remove")}
        </Button>
      </div>
      <Show when={!removing() && s.error()}>
        {(e) => (
          <p class="field-error" role="alert">
            {e()}
          </p>
        )}
      </Show>
      <ServiceRemoveDialog
        open={removing()}
        busy={s.busy()}
        onConfirm={(d) => void remove(d)}
        onCancel={() => setRemoving(false)}
      />
    </>
  );
}
