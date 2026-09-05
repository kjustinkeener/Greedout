// Catalogs are imported statically, not `import()`ed per locale. Fourteen
// languages of ~110 keys is a few tens of kB inside an exe that already ships
// offline, and static keeps `t()` synchronous: no loading state and no flash of
// untranslated text on a language switch. Revisit at a few thousand keys.

import { en } from "./en";
import type { PartialDict } from "./en";
import { de } from "./de";
import { es } from "./es";
import { fr } from "./fr";
import { it } from "./it";
import { nl } from "./nl";
import { pl } from "./pl";
import { ptBR } from "./pt-BR";
import { ru } from "./ru";
import { tr } from "./tr";
import { ja } from "./ja";
import { ko } from "./ko";
import { zhHans } from "./zh-Hans";
import { zhHant } from "./zh-Hant";

export const CATALOGS: Record<string, PartialDict> = {
  en,
  de,
  es,
  fr,
  it,
  nl,
  pl,
  "pt-BR": ptBR,
  ru,
  tr,
  ja,
  ko,
  "zh-Hans": zhHans,
  "zh-Hant": zhHant,
};
