// Tooltip copy for the readouts, in one place so a number means the same thing
// wherever it appears (a row, the focus panel, the Explorer).
//
// Functions, not consts. A `const` capturing `t()` is evaluated once at module
// load and would freeze the tooltips in whatever language was active then, so
// switching the language would relabel the UI and leave the tooltips behind.

import { t } from "./i18n.svelte";

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

export const agoTip = (): string => t("tip.ago");
export const ctxTip = (): string => t("tip.ctx");
export const pctTip = (): string => t("tip.pct");
export const modelTip = (): string => t("tip.model");
export const sizeTip = (): string => t("tip.size");
export const limitsTip = (): string => t("tip.limits");
