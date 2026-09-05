import type { PartialDict } from "./en";

export const es: PartialDict = {
  // Menú de la barra de título y controles de ventana
  "menu.menu": "Menú",
  "menu.closeMenu": "Cerrar menú",
  "menu.explorer": "Explorador de contexto…",
  "menu.settings": "Configuración…",
  "menu.about": "Acerca de…",
  "win.minimize": "Minimizar",
  "win.close": "Cerrar",

  // La lista de indicadores
  "main.noSessions": "No hay sesiones activas",
  "main.renamePrompt": "Cambiar nombre a “{title}”",
  "main.exploreOrRename": "Clic para explorar · doble clic para renombrar",
  "main.rename": "Doble clic para renombrar",
  "main.exploreProject": "Explorar este proyecto",
  "main.limits": "objetivo {target} · máx {max}",
  "main.spend": "gasto",
  "main.context": "contexto",
  "main.startupFloor": "base inicial",

  // Tiempo compacto desde el último mensaje. Deliberadamente corto: caben en
  // una ventana de 340px junto al nombre del proyecto.
  "ago.seconds": "{n}s",
  "ago.minutes": "{n}m",
  "ago.hours": "{n}h",
  "ago.days": "{n}d",
  "ago.weeks": "{n}sem",

  // Descripciones emergentes. Un solo lugar, para que un número signifique lo
  // mismo dondequiera que aparezca: una fila, el panel de enfoque, el Explorador.
  "tip.money":
    "Nominal: estimado a partir de los tokens de esta sesión según los precios por token publicados, calculado por turno según el modelo que lo ejecutó. En un plan de tarifa plana no se te cobra esto; es lo que costaría el mismo trabajo a través de la API.",
  "tip.spend": "{amount} hasta ahora. {money}",
  "tip.ago": "Tiempo transcurrido desde el último mensaje de esta sesión",
  "tip.ctx":
    "Contexto acumulado en el mensaje más reciente, respecto al objetivo al que está escalado el indicador",
  "tip.pct": "Contexto en el mensaje más reciente, como porcentaje del objetivo",
  "tip.model": "Modelo que ejecutó el turno más reciente",
  "tip.size": "Tamaño en disco del archivo de transcripción de esta sesión",
  "tip.limits": "El objetivo es donde el indicador entra en zona roja; el máximo es el límite de contexto del modelo",
  "tip.gauge": "indicador de llenado de contexto",
  "tip.history": "contexto y gasto a lo largo del tiempo",

  // Barra de estado
  "status.cpu": "CPU",
  "status.mem": "MEM",
  "status.cpuTip": "uso de CPU por núcleo",
  "status.memTip": "memoria {used} / {total} GB",

  // Ventana de configuración
  "settings.title": "Configuración",
  "settings.followFocus": "Seguir la sesión abierta en Claude",
  "settings.alwaysOnTop": "Siempre visible",
  "settings.showInTray": "Mostrar en la bandeja",
  "settings.showInTaskbar": "Mostrar en la barra de tareas",
  "settings.minimizeToTray": "Minimizar a la bandeja",
  "settings.closeToTray": "Cerrar a la bandeja",
  "settings.enableExplorer": "Activar el Explorador de contexto",
  "settings.showStatusbar": "Mostrar barra de estado de CPU / memoria",
  "settings.checkUpdates": "Buscar actualizaciones al iniciar",
  "settings.debugLogging": "Registro de depuración en archivo",
  "settings.lockoutTip":
    "No se puede desactivar: es la única forma que queda de llegar a esta ventana. Activa primero la otra.",
  "settings.language": "Idioma",
  "settings.languageAuto": "Automático (sistema)",
  "settings.theme": "Tema",
  "settings.fonts": "Fuentes",
  "settings.boldness": "Grosor",
  "settings.opacity": "Opacidad",
  "settings.maxSessions": "Máximo de sesiones mostradas",
  "settings.refreshEvery": "Actualizar cada",
  "settings.dimAfter": "Atenuado por completo después de",
  "settings.budget": "Presupuesto predeterminado (k tokens)",
  "settings.budgetHint": "Se usa solo cuando no se reconoce el modelo de una sesión.",
  "settings.resetAll": "Restablecer todo",
  "settings.resetAllTip": "Restablecer cada opción a su valor predeterminado",

  // Los cinco niveles del control de grosor, del más fino al más grueso.
  "bold.lightest": "Más fino",
  "bold.lighter": "Fino",
  "bold.normal": "Normal",
  "bold.bolder": "Grueso",
  "bold.boldest": "Más grueso",

  "unit.sec": "seg",
  "unit.hr": "h",

  // Ventana Acerca de
  "about.version": "Versión {version}",
  "about.built": "compilado el {date}",
  "about.tagline": "Lectura del gasto de tokens y explorador de contexto.",
  "about.body":
    "Greedout lee directamente las transcripciones de Claude Code y muestra el llenado de contexto, el gasto y el historial de cada sesión como un panel siempre visible: la barra de estado que la app de escritorio de Windows no puede mostrar por sí sola.",
  "about.madeBy": "Creado por",
  "about.builtWith": "Hecho con",
  "about.license": "Licencia",
  "about.notices": "Avisos de terceros",

  "install.heading": "Instalar",
  "install.update": "Actualizar",
  "install.location": "Ubicación",
  "install.desktopShortcut": "Añadir también un acceso directo en el escritorio",
  "install.working": "Instalando…",
  "install.failed": "Error de instalación: {error}",
  "install.retry": "Reintentar",
  "install.starting": "Instalado. Iniciando greedout…",
  "install.pokeTip": "Pasa el cursor para acelerar · haz clic para cambiar de color",

  "update.available": "La versión {version} está disponible. Tienes la {current}.",
  "update.install": "Descargar e instalar",
  "update.downloading": "Descargando {version}…",
  "update.installed": "Instalado. Reiniciando…",
  "update.failed": "Error al actualizar: {error}",
  "update.dismiss": "Ahora no",
  "update.check": "Buscar actualizaciones",
  "update.checking": "Comprobando…",
  "update.upToDate": "Tienes la última versión.",

  "common.close": "Cerrar",
  "common.loading": "Cargando…",
} as const;
