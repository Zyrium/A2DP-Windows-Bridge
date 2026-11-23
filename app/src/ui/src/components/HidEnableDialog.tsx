// Turning on keyboards and mice through BlueCaster (experimental, issue
// #413). Dangerous: their input exists only while BlueCaster runs, so the
// dialog asks for more than a click. Both statements ticked, the core's
// code typed back (no paste), and a wait of HID_WAIT_SECONDS, so that the
// risks are on screen long enough to be read. The core checks the code
// and the statements again (hid.enable).

import { createEffect, createSignal, createUniqueId, For, on, onCleanup, Show } from "solid-js";
import { errorText, useApp } from "../app/context";
import { t } from "../i18n";
import { Dialog } from "./controls";
import { Icon } from "./Icon";

/** How long the risks stay on screen before Turn on can be pressed */
export const HID_WAIT_SECONDS = 10;

const RISKS = ["ui.dev.hid_risk_close", "ui.dev.hid_risk_signin", "ui.dev.hid_risk_admin", "ui.dev.hid_risk_games"];
// Through the service they also work on the sign-in screen and as administrator
const RISKS_SERVICE = ["ui.dev.hid_risk_service", "ui.dev.hid_risk_sas", "ui.dev.hid_risk_games"];

export function HidEnableDialog(props: { open: boolean; onClose: (enabled: boolean) => void }) {
  const app = useApp();
  const id = createUniqueId();
  const [code, setCode] = createSignal<string | null>(null);
  const [typed, setTyped] = createSignal("");
  const [otherInput, setOtherInput] = createSignal(false);
  const [risk, setRisk] = createSignal(false);
  const [left, setLeft] = createSignal(HID_WAIT_SECONDS);
  const [error, setError] = createSignal<string | null>(null);
  const [busy, setBusy] = createSignal(false);
  const service = () => !!app.core.state.snap?.service_mode;

  const fetchCode = async () => {
    setCode(null);
    try {
      setCode((await app.core.request("hid.code")).code);
    } catch (e) {
      setError(errorText(e));
    }
  };

  // Each opening starts over: nothing ticked, a new code, the full wait
  createEffect(
    on(
      () => props.open,
      (open) => {
        if (!open) return;
        setTyped("");
        setOtherInput(false);
        setRisk(false);
        setError(null);
        setLeft(HID_WAIT_SECONDS);
        void fetchCode();
        const opened = Date.now();
        const timer = setInterval(() => {
          const l = Math.max(0, Math.ceil(HID_WAIT_SECONDS - (Date.now() - opened) / 1000));
          setLeft(l);
          if (l === 0) clearInterval(timer);
        }, 250);
        onCleanup(() => clearInterval(timer));
      },
    ),
  );

  const digits = () => typed().replace(/\D/g, "");
  const ready = () => otherInput() && risk() && digits().length === 6 && left() === 0 && code() !== null;

  const confirm = async () => {
    if (!ready() || busy()) return;
    setBusy(true);
    setError(null);
    try {
      await app.core.request("hid.enable", {
        code: digits(),
        other_input_at_hand: otherInput(),
        risk_understood: risk(),
      });
      setBusy(false);
      props.onClose(true);
    } catch (e) {
      // A code counts once: a wrong or expired one needs a new one
      setBusy(false);
      setError(errorText(e));
      setTyped("");
      void fetchCode();
    }
  };

  // Typed, not pasted or dropped in
  const block = (e: Event) => e.preventDefault();

  return (
    <Dialog
      open={props.open}
      title={t("ui.dev.hid_enable_title")}
      warn
      danger
      confirm={left() > 0 ? t("ui.dev.hid_enable_wait", left()) : t("ui.dev.hid_enable_confirm")}
      confirmDisabled={!ready()}
      cancel={t("modal.cancel")}
      busy={busy()}
      onConfirm={() => void confirm()}
      onCancel={() => props.onClose(false)}
    >
      <div class="hid-enable">
        <p class="hid-lead">{t(service() ? "ui.dev.hid_enable_lead_service" : "ui.dev.hid_enable_lead")}</p>
        <ul class="hid-risks">
          <For each={service() ? RISKS_SERVICE : RISKS}>{(k) => <li>{t(k)}</li>}</For>
        </ul>
        <label class="check-item">
          <input type="checkbox" checked={otherInput()} onChange={(e) => setOtherInput(e.currentTarget.checked)} />
          <span>{t("ui.dev.hid_confirm_other")}</span>
        </label>
        <label class="check-item">
          <input type="checkbox" checked={risk()} onChange={(e) => setRisk(e.currentTarget.checked)} />
          <span>{t("ui.dev.hid_confirm_risk")}</span>
        </label>
        <div class="hid-code-box">
          <span id={`${id}-prompt`}>{t("ui.dev.hid_code_prompt")}</span>
          <span class="hid-code mono" aria-live="polite" data-testid="hid-code">
            {code() ?? "······"}
          </span>
          <input
            id={id}
            aria-label={t("ui.dev.hid_code_input")}
            class="input mono hid-code-input"
            type="text"
            inputmode="numeric"
            autocomplete="off"
            spellcheck={false}
            maxLength={6}
            value={typed()}
            aria-describedby={`${id}-prompt${error() ? ` ${id}-err` : ""}`}
            aria-invalid={error() ? "true" : undefined}
            onInput={(e) => setTyped(e.currentTarget.value)}
            onPaste={block}
            onDrop={block}
            onKeyDown={(e) => e.key === "Enter" && void confirm()}
          />
          <Show when={error()}>
            <span class="field-error" id={`${id}-err`} role="alert">
              <Icon name="error_circle" />
              {error()}
            </span>
          </Show>
        </div>
      </div>
    </Dialog>
  );
}
