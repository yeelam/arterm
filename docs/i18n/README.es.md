# arTerm: terminal remoto persistente de Windows tras perder la conexión

<a id="languages"></a>
<details>
<summary>Languages / 语言 / 言語 / اللغات (16)</summary>

[English](../../README.md) | [简体中文](README.zh-CN.md) | [繁體中文](README.zh-TW.md) | [日本語](README.ja.md) | [한국어](README.ko.md) | [Español](README.es.md) | [Português (Brasil)](README.pt-BR.md) | [Français](README.fr.md) | [Deutsch](README.de.md) | [Italiano](README.it.md) | [Русский](README.ru.md) | [Türkçe](README.tr.md) | [Tiếng Việt](README.vi.md) | [Bahasa Indonesia](README.id.md) | [हिन्दी](README.hi.md) | [العربية](README.ar.md)

</details>

Mantén compilaciones y agentes de programación en marcha si se pierde el cliente o la VPN; vuelve al mismo shell interactivo, variables y directorio; consulta la salida retenida: reproducción limitada, no un registro completo. Ambos extremos Windows; host encendido y usuario conectado. Sin recuperación tras reinicio o cierre de sesión del host.

[Configurar](../../README.md#get-connected) · [Verificar descarga y confianza](../../DEVELOPMENT-INSTALL.md) · [Probar el mismo shell](../../README.md#prove-that-you-returned-to-the-same-shell)

<img src="../../.github/arterm-before-after.png" width="700" alt="Antes y después: tras cerrar el cliente, perder la VPN o reiniciar el portátil, vuelve al mismo shell remoto, variables y trabajo; el host sigue encendido con el usuario conectado.">

*Ilustración del flujo, no captura de una sesión real ni prueba de ejecución. Las etiquetas de la imagen están en inglés. La idea: después de una desconexión del cliente, recuperas el mismo shell, variables y trabajo en el host, siempre que siga encendido y con el usuario conectado. No conserva procesos tras reiniciar el host.*

- **El trabajo sigue aunque el cliente se desconecte.** Compilaciones, agentes y comandos largos siguen en el host Windows remoto al cerrar el cliente, perder la VPN o reiniciar el portátil.
- **Vuelve a tu trabajo, no a un shell vacío.** Reconecta al mismo proceso de shell interactivo, con sus variables y directorio de trabajo.
- **Consulta lo ocurrido durante tu ausencia.** La reconexión reproduce la salida retenida del terminal; la retención es limitada, puede haber huecos y no es un registro completo.

Para tareas no interactivas habituales, tu canal de comandos remotos ya aprobado puede bastar. Elige arTerm si necesitas volver al **mismo shell interactivo de Windows y su entorno, ejecutados en el host**, tras cerrar el cliente, perder la VPN o reiniciar el portátil, respetando sus requisitos de conexión y controles locales documentados. Ambos extremos deben ser Windows y el host debe seguir encendido y con la sesión iniciada; no recupera procesos tras reiniciar el host.


Pierde la conexión, no tu trabajo. Deja compilaciones, comandos de larga duración y agentes de programación en el Windows remoto. Tras cerrar el cliente, perder la VPN o reiniciar el portátil, vuelve al mismo shell sin empezar de cero.

El host remoto mantiene el shell en ejecución; tu portátil solo se conecta a él.

Encaja si ambos equipos usan Windows, los túneles usan la misma cuenta de GitHub y se permite tráfico saliente. El host debe seguir encendido con el usuario conectado. No restaura procesos tras reinicio, cierre de sesión, fallo o apagado del host ni exit del shell. Los paquetes actuales tienen firma de desarrollo, no son versiones de producción de confianza pública.

[Verifica la descarga y revisa la decisión de confianza antes de instalar](../../DEVELOPMENT-INSTALL.md). No eludas avisos del sistema ni políticas de tu organización.

[Descargar x64 / ARM64](https://github.com/yeelam/arterm/releases/latest) · [Configurar](../../README.md#get-connected) → [Comprobar la reconexión al mismo shell](../../README.md#prove-that-you-returned-to-the-same-shell)

## Requisitos y confianza

Ambos equipos necesitan Windows. El host debe seguir encendido con el usuario conectado. El cliente usa Microsoft devtunnel CLI; el host, una CLI nativa compatible de túneles de VS Code, como code-tunnel.exe. El editor completo es opcional. Ambos túneles necesitan la misma cuenta de GitHub; la autenticación de gh es distinta. Se requiere tráfico saliente permitido, consentimiento para descargar dependencias y aceptación de la licencia de VS Code server.

Elige x64 o ARM64 en [Descargas para Windows](https://github.com/yeelam/arterm/releases/latest). La ruta verificada redirige a v0.7.1. Los paquetes llevan firma de desarrollo, no un certificado de producción de confianza pública. Sigue la [guía de instalación](../../DEVELOPMENT-INSTALL.md) para comprobar sumas y huella del certificado público. Solo añade confianza para el usuario actual con aprobación explícita y permiso de tu organización. No eludas Authenticode, SmartScreen ni controles de aplicaciones.

**Antes de añadir confianza o instalar:** sigue la [comparación del ZIP](../../DEVELOPMENT-INSTALL.md#verify-the-downloaded-release-payload-before-trust-or-installation) con Get-FileHash y la suma del archivo publicada en la versión. La huella CER no verifica el ZIP ni los ejecutables. Detente si falta la suma pública o no coincide; una coincidencia no garantiza seguridad ni aprueba confianza.



El host también requiere Windows Script Host/VBScript permitido por la política. Compruébalo antes de confiar o instalar; si está bloqueado, detente sin eludirla.

## Configuración

**Mantén privados los datos de sesión:** la salida retenida, el historial normal del shell y los archivos de recuperación pueden contener comandos, rutas, código o secretos. Protégelos en ambos equipos y elimina información sensible antes de compartir registros, capturas o paquetes de soporte. DPAPI protege credenciales, no garantiza que todos los archivos estén cifrados o sin contenido. Las exclusiones de contenido de los diagnósticos de disponibilidad son más limitadas.

Antes de ejecutar, usa Get-AuthenticodeSignature según la [inspección de solo lectura](../../DEVELOPMENT-INSTALL.md#inspect-extracted-executable-signatures-without-running-them) de instaladores y clientes extraídos. Suma ZIP, identidad CER y confianza Windows son comprobaciones distintas. Exige el firmante de desarrollo fijado y estado Valid; un firmante desconocido o estado no Valid exige parar, no eludir avisos. La confianza requiere aprobación explícita y permiso de política por separado, y después repetir la inspección. El control se limita al mismo usuario local, sesión, integridad/elevación y cliente firmado idéntico; no es compartir agentes arbitrarios entre máquinas.

En el host ejecuta arTerm-Host-Setup.exe y abre un nuevo PowerShell:

```powershell
arterm-host setup --name my-devbox
```

Completa el inicio de sesión del túnel y guarda el comando de registro mostrado. Instala arTerm-Client-Setup.exe localmente, abre un nuevo PowerShell y usa la misma cuenta de GitHub. Ejecuta exactamente el registro impreso por el host con su ruta real. El instalador inicializa el cliente; no hace falta arterm setup. Con un nombre de sesión nuevo, conecta desde el equipo local:

```powershell
arterm connect my-devbox MyProof --shell powershell
```

## Comprobar el mismo shell

En el PowerShell remoto, ejecuta y anota el PID y el directorio:

```powershell
$artermProof = 'kept-on-host'
$artermPid = $PID
$artermCwd = (Get-Location).Path
$PID; $artermProof; (Get-Location).Path
```

Pulsa Ctrl+] para separar el cliente. Si el terminal captura el atajo, cierra solo la pestaña del cliente local. Mantén el host encendido y conectado a Windows. Repite localmente el mismo comando:

```powershell
arterm connect my-devbox MyProof --shell powershell
```

En el PowerShell remoto recuperado:

```powershell
$PID; $artermProof; (Get-Location).Path
($PID -eq $artermPid) -and ($artermProof -eq 'kept-on-host') -and ((Get-Location).Path -eq $artermCwd)
```

El resultado esperado es el mismo PID, kept-on-host, el mismo directorio y True. Usa ese comando para continuar en futuras conexiones. Para terminar la prueba, escribe exit en el shell remoto: finaliza el proceso y su estado ya no se recupera.

## Límites

No se recuperan procesos tras reiniciar, cerrar sesión, apagar o sufrir un fallo del host, ni tras salir del shell. El host arranca al iniciar sesión; no configura login automático ni un servicio sin usuario conectado. La reproducción de salida es limitada. La rama de desarrollo completa no está cualificada para publicación: siguen pendientes la revisión independiente de archivos de transferencia y los criterios de entrada directa. No se certifican TUI arbitrarias; desbloquear archivos automáticamente no garantiza su seguridad.



La reproducción es limitada; guarda un registro de compilación remoto para conservar toda la salida. arterm-host stop puede reiniciar automáticamente; usa arterm-host stop --disable para mantener la parada.

## Recuperación y reversión

Si falla, ejecuta arterm doctor my-devbox y revisa cuentas y red; usa arterm --login para recuperar explícitamente la autenticación del cliente si hace falta. No borres datos de recuperación ni reinstales un host activo. Las actualizaciones rechazan por defecto interrumpir sesiones activas. En el host, arterm-host stop --disable mantiene la parada; arterm-host start lo reinicia sin recuperar shells perdidos. La desinstalación conserva datos; la guía explica cómo retirar la confianza del certificado.

## Control local avanzado

send/read gestionados requieren una sesión PowerShell/pwsh compatible recién creada y la identidad local de conexión de confianza coincidente. No se adapta un shell existente. La publicación no cierra los criterios de entrada directa/revisión de archivos transferidos de la rama; versiones desconocidas siguen sin verificar.



Usuarios de Copilot CLI, Claude Code, Codex, Gemini CLI, Kimi y Qwen CLI pueden consultar y ejecutar comandos normales de Windows si sus herramientas lo permiten. No es una certificación de integración nativa de seis clientes. Otro terminal de control local exige el mismo usuario, sesión y contexto de integridad/elevación, con un cliente firmado de confianza e idéntico byte a byte. Mantén la conexión en ejecución.

[README](../../README.md) · [Quick start](../../QUICKSTART.md) · [Automation](../../QUICKSTART.md#automation) · [Solución de problemas](../../README.md#installation-and-troubleshooting-details) · [Actualizar y detener](../../QUICKSTART.md#upgrade-without-registering-again) · [Retirar la confianza del certificado](../../DEVELOPMENT-INSTALL.md) · [Release evidence / gates](../../README.md#released-downloads-versus-development-gates) · [Signing](../../SIGNING.md)
