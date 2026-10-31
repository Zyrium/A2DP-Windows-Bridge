// The texts of the Fones screen: the "fones.*" keys of app/lang (en.json
// and pt-BR.json; the other languages show English), so the screen
// follows the app language like every other screen.

import { t } from "../../i18n";

export function ft(key: string, ...args: (string | number)[]): string {
  return t(`fones.${key}`, ...args);
}
