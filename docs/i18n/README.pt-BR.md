# arTerm: terminal remoto persistente do Windows após desconexão

[English](../../README.md) | [简体中文](README.zh-CN.md) | [日本語](README.ja.md) | [Español](README.es.md) | [Português (Brasil)](README.pt-BR.md) | [Français](README.fr.md) | [Deutsch](README.de.md)

Perca a conexão, não o trabalho. Deixe compilações, comandos demorados e agentes de programação no Windows remoto. Depois de fechar o cliente, perder a VPN ou reiniciar o notebook, volte ao mesmo shell sem começar do zero.

Cliente e host precisam usar Windows. O host deve continuar ligado e com o usuário conectado. O cliente usa Microsoft devtunnel CLI; o host, uma CLI nativa compatível de túneis do VS Code, como code-tunnel.exe. O editor completo é opcional. Ambos os túneis precisam da mesma conta do GitHub; a autenticação do gh é separada. É preciso permitir conexões de saída, consentir com downloads de dependências e aceitar a licença do VS Code server.

Escolha x64 ou ARM64 em [Downloads para Windows](https://github.com/yeelam/arterm/releases/latest). A rota verificada redireciona para v0.7.1. Os pacotes têm assinatura de desenvolvimento, não um certificado de produção com confiança pública. Siga o [guia de instalação](../../DEVELOPMENT-INSTALL.md) para conferir checksums e a impressão digital do certificado público. Só adicione confiança ao usuário atual com aprovação explícita e permissão da organização. Não contorne Authenticode, SmartScreen ou controles de aplicativos.

**Antes de adicionar confiança ou instalar:** siga a [comparação do ZIP](../../DEVELOPMENT-INSTALL.md#verify-the-downloaded-release-payload-before-trust-or-installation) com Get-FileHash e o checksum do arquivo publicado na release. A impressão digital do CER não verifica o ZIP nem os executáveis. Pare se o checksum público estiver ausente ou não corresponder; uma correspondência não garante segurança nem aprova confiança.

**Mantenha os dados da sessão privados:** saída retida, histórico normal do shell e arquivos de recuperação podem conter comandos, caminhos, código ou segredos. Proteja-os nos dois computadores e remova informações sensíveis antes de compartilhar logs, capturas ou pacotes de suporte. A proteção de credenciais por DPAPI não significa que todos os arquivos sejam criptografados ou sem conteúdo. As exclusões de conteúdo dos diagnósticos de prontidão são mais restritas.

No host, execute arTerm-Host-Setup.exe e abra um novo PowerShell:

```powershell
arterm-host setup --name my-devbox
```

Conclua o login do túnel e guarde o comando de registro mostrado. Instale arTerm-Client-Setup.exe localmente, abra um novo PowerShell e use a mesma conta do GitHub. Execute exatamente o registro impresso pelo host com o caminho real informado. O instalador já inicializa o cliente; arterm setup não é necessário. Com um nome de sessão novo, conecte localmente:

```powershell
arterm connect my-devbox MyProof --shell powershell
```

No PowerShell remoto, execute e anote o PID e a pasta:

```powershell
$artermProof = 'kept-on-host'
$artermPid = $PID
$artermCwd = (Get-Location).Path
$PID; $artermProof; (Get-Location).Path
```

Pressione Ctrl+] para desconectar apenas o cliente. Se o terminal capturar o atalho, feche só a aba do cliente local. Mantenha o host ligado e o usuário conectado. Repita localmente o mesmo comando:

```powershell
arterm connect my-devbox MyProof --shell powershell
```

No PowerShell remoto reconectado:

```powershell
$PID; $artermProof; (Get-Location).Path
($PID -eq $artermPid) -and ($artermProof -eq 'kept-on-host') -and ((Get-Location).Path -eq $artermCwd)
```

O esperado é o mesmo PID, kept-on-host, a mesma pasta e True. Use o mesmo comando nas próximas conexões para continuar o trabalho. Para encerrar o teste, digite exit no shell remoto: o processo termina e seu estado não poderá ser retomado.

Não há recuperação de processos após reinicialização, logoff, falha ou desligamento do host, nem após saída do shell. O host inicia no login do usuário; não configura login automático nem serviço sem usuário conectado. A reprodução de saída tem retenção limitada. A branch de desenvolvimento inteira não está qualificada para release: a revisão independente dos arquivos de transferência e os critérios de entrada direta continuam pendentes. TUIs arbitrárias não são certificadas; desbloquear arquivos automaticamente não significa que sejam seguros.

Se falhar, execute arterm doctor my-devbox e confira contas e rede; use arterm --login para recuperar explicitamente o login do cliente quando necessário. Não apague registros de recuperação nem reinstale um host em execução. Atualizações não interrompem sessões ativas por padrão. No host, arterm-host stop --disable mantém o host parado; arterm-host start inicia novamente, mas não recupera shells perdidos. A desinstalação mantém dados; o guia explica como remover a confiança no certificado.

Usuários de Copilot CLI, Claude Code, Codex, Gemini CLI, Kimi e Qwen CLI podem ler e executar comandos comuns do Windows quando suas ferramentas permitirem. Isso não declara integração nativa nem certificação dos seis clientes. Outro terminal de controle local exige o mesmo usuário, sessão e contexto de integridade/elevação, além de cliente assinado confiável e idêntico byte a byte. Mantenha a conexão em execução.

[README](../../README.md) · [Quick start](../../QUICKSTART.md) · [Automation](../../QUICKSTART.md#automation) · [Troubleshooting](../../README.md#installation-and-troubleshooting-details) · [Upgrade / stop](../../QUICKSTART.md#upgrade-without-registering-again) · [Trust removal](../../DEVELOPMENT-INSTALL.md) · [Release evidence / gates](../../README.md#released-downloads-versus-development-gates) · [Signing](../../SIGNING.md)
