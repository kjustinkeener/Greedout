// Tooltip copy for the readouts, in one place so a number means the same thing
// wherever it appears (a row, the focus panel, the Explorer).
//
// Functions, not consts. A `const` capturing `t()` is evaluated once at module
// load and would freeze the tooltips in whatever language was active then, so
// switching the language would relabel the UI and leave the tooltips behind.

import { t } from "./i18n.svelte";
import type { LimitWindow } from "../types";

/**
 * Money on screen is derived, never billed. Each turn is priced from its own
 * token counts at its own model's published per-token rate, so a session on a
 * flat-rate plan still shows a figure: what the same work would have cost
 * through the API. Saying so in the tooltip matters because the number is
 * otherwise easy to read as an amount actually charged.
 */
export const moneyTip = (): string => t("tip.money");

export const spendTip = (usd: number): string =>
  t("tip.spend", { amount: `$${usd.toFixed(2)}`, money: moneyTip() });

const pctStr = (f: number) => `${Math.round(f * 100)}%`;
const leftStr = (ms: number) => {
  const m = Math.max(0, Math.round(ms / 60000));
  return m >= 1440 ? `${Math.floor(m / 1440)}d ${Math.floor((m % 1440) / 60)}h` : `${Math.floor(m / 60)}h ${m % 60}m`;
};

/** Tooltip for one plan-limit dial: the estimate and its budget, or how to
 *  calibrate when there is none yet. */
export const limitTip = (kind: "5h" | "week", w: LimitWindow): string => {
  const name = t(kind === "5h" ? "tip.limit5h" : "tip.limitWeek");
  const lines: string[] = [
    w.pct != null
      ? t("tip.limitEst", { name, pct: pctStr(w.pct), cost: `$${w.cost.toFixed(2)}`, budget: `$${w.budget.toFixed(0)}` })
      : t("tip.limitUncal", { name }),
  ];
  lines.push(
    w.elapsed != null && w.resetsAt != null
      ? t("tip.limitElapsed", { pct: pctStr(w.elapsed), left: leftStr(w.resetsAt - Date.now()) })
      : t("tip.limitNoClock"),
  );
  lines.push(t("tip.limitHow"), t("tip.limitClick"));
  return lines.join("\n");
};

export const agoTip = (): string => t("tip.ago");
export const ctxTip = (): string => t("tip.ctx");
export const pctTip = (): string => t("tip.pct");
export const modelTip = (): string => t("tip.model");
export const sizeTip = (): string => t("tip.size");
export const limitsTip = (): string => t("tip.limits");
