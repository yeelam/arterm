# arTerm: terminal remoto persistente de Windows tras perder la conexión

[English](../../README.md) | [简体中文](README.zh-CN.md) | [日本語](README.ja.md) | [Español](README.es.md) | [Português (Brasil)](README.pt-BR.md) | [Français](README.fr.md) | [Deutsch](README.de.md)

Pierde la conexión, no tu trabajo. Deja compilaciones, comandos largos y agentes de programación en el Windows remoto. Tras cerrar el cliente, perder la VPN o reiniciar el portátil, vuelve al mismo shell sin empezar de cero.

Ambos equipos necesitan Windows. El host debe seguir encendido con el usuario conectado. El cliente usa Microsoft devtunnel CLI; el host, una CLI nativa compatible de túneles de VS Code, como code-tunnel.exe. El editor completo es opcional. Ambos túneles necesitan la misma cuenta de GitHub; la autenticación de gh es distinta. Se requiere tráfico saliente permitido, consentimiento para descargar dependencias y aceptación de la licencia de VS Code server.

Elige x64 o ARM64 en [Descargas para Windows](https://github.com/yeelam/arterm/releases/latest). La ruta verificada redirige a v0.7.1. Los paquetes llevan firma de desarrollo, no un certificado de producción de confianza pública. Sigue la [guía de instalación](../../DEVELOPMENT-INSTALL.md) para comprobar sumas y huella del certificado público. Solo añade confianza para el usuario actual con aprobación explícita y permiso de tu organización. No eludas Authenticode, SmartScreen ni controles de aplicaciones.

**Antes de añadir confianza o instalar:** sigue la [comparación del ZIP](../../DEVELOPMENT-INSTALL.md#verify-the-downloaded-release-payload-before-trust-or-installation) con Get-FileHash y la suma del archivo publicada en la versión. La huella CER no verifica el ZIP ni los ejecutables. Detente si falta la suma pública o no coincide; una coincidencia no garantiza seguridad ni aprueba confianza.

**Mantén privados los datos de sesión:** la salida retenida, el historial normal del shell y los archivos de recuperación pueden contener comandos, rutas, código o secretos. Protégelos en ambos equipos y elimina información sensible antes de compartir registros, capturas o paquetes de soporte. DPAPI protege credenciales, no garantiza que todos los archivos estén cifrados o sin contenido. Las exclusiones de contenido de los diagnósticos de disponibilidad son más limitadas.

En el host ejecuta arTerm-Host-Setup.exe y abre un nuevo PowerShell:

```powershell
arterm-host setup --name my-devbox
```

Completa el inicio de sesión del túnel y guarda el comando de registro mostrado. Instala arTerm-Client-Setup.exe localmente, abre un nuevo PowerShell y usa la misma cuenta de GitHub. Ejecuta exactamente el registro impreso por el host con su ruta real. El instalador inicializa el cliente; no hace falta arterm setup. Con un nombre de sesión nuevo, conecta desde el equipo local:

```powershell
arterm connect my-devbox MyProof --shell powershell
```

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

No se recuperan procesos tras reiniciar, cerrar sesión, apagar o sufrir un fallo del host, ni tras salir del shell. El host arranca al iniciar sesión; no configura login automático ni un servicio sin usuario conectado. La reproducción de salida es limitada. La rama de desarrollo completa no está cualificada para publicación: siguen pendientes la revisión independiente de archivos de transferencia y los criterios de entrada directa. No se certifican TUI arbitrarias; desbloquear archivos automáticamente no garantiza su seguridad.

Si falla, ejecuta arterm doctor my-devbox y revisa cuentas y red; usa arterm --login para recuperar explícitamente la autenticación del cliente si hace falta. No borres datos de recuperación ni reinstales un host activo. Las actualizaciones rechazan por defecto interrumpir sesiones activas. En el host, arterm-host stop --disable mantiene la parada; arterm-host start lo reinicia sin recuperar shells perdidos. La desinstalación conserva datos; la guía explica cómo retirar la confianza del certificado.

Usuarios de Copilot CLI, Claude Code, Codex, Gemini CLI, Kimi y Qwen CLI pueden consultar y ejecutar comandos normales de Windows si sus herramientas lo permiten. No es una certificación de integración nativa de seis clientes. Otro terminal de control local exige el mismo usuario, sesión y contexto de integridad/elevación, con un cliente firmado de confianza e idéntico byte a byte. Mantén la conexión en ejecución.

[README](../../README.md) · [Quick start](../../QUICKSTART.md) · [Automation](../../QUICKSTART.md#automation) · [Troubleshooting](../../README.md#installation-and-troubleshooting-details) · [Upgrade / stop](../../QUICKSTART.md#upgrade-without-registering-again) · [Trust removal](../../DEVELOPMENT-INSTALL.md) · [Release evidence / gates](../../README.md#released-downloads-versus-development-gates) · [Signing](../../SIGNING.md)
