# arTerm: terminal remoto persistente do Windows após desconexão

<a id="languages"></a>
<details>
<summary>Languages / 语言 / 言語 / اللغات (16)</summary>

[English](../../README.md) | [简体中文](README.zh-CN.md) | [繁體中文](README.zh-TW.md) | [日本語](README.ja.md) | [한국어](README.ko.md) | [Español](README.es.md) | [Português (Brasil)](README.pt-BR.md) | [Français](README.fr.md) | [Deutsch](README.de.md) | [Italiano](README.it.md) | [Русский](README.ru.md) | [Türkçe](README.tr.md) | [Tiếng Việt](README.vi.md) | [Bahasa Indonesia](README.id.md) | [हिन्दी](README.hi.md) | [العربية](README.ar.md)

</details>

Mantenha builds e agentes de programação rodando se o cliente ou a VPN desconectar; volte ao mesmo shell interativo, variáveis e diretório; veja a saída retida: reprodução limitada, não um log completo. Windows nas duas pontas; host ligado e usuário conectado. Sem recuperação após reiniciar o host ou encerrar sua sessão Windows.

[Configurar](#native-setup) · [Verificar download e confiança](../../DEVELOPMENT-INSTALL.md) · [Comprovar o mesmo shell](../../README.md#prove-that-you-returned-to-the-same-shell) · [Referência técnica (inglês)](../../README.md#get-connected)

<img src="../../.github/arterm-before-after.png" width="700" alt="Antes e depois: após fechar o cliente, perder a VPN ou reiniciar o notebook, volte ao mesmo shell remoto, variáveis e trabalho; o host continua ligado com o usuário conectado.">

*Ilustração do fluxo, não captura de sessão real nem prova de execução. Os rótulos da imagem estão em inglês. A ideia é voltar ao mesmo shell, variáveis e trabalho após a desconexão do cliente, enquanto o host continua ligado e o usuário conectado. Não preserva processos após reiniciar o host.*

- **O trabalho continua sem o cliente.** Builds, agentes e comandos demorados continuam no host Windows remoto ao fechar o cliente, perder a VPN ou reiniciar o notebook.
- **Volte ao trabalho, não a um shell vazio.** Reconecte ao mesmo processo de shell interativo, preservando variáveis e diretório de trabalho.
- **Veja o que aconteceu enquanto estava fora.** A reconexão reproduz a saída retida do terminal; a retenção é limitada, pode haver lacunas e não é um log completo.

Para tarefas não interativas comuns, seu canal de comandos remotos já aprovado pode ser suficiente. Escolha arTerm quando precisar se reconectar ao **mesmo shell interativo Windows e seu ambiente, executados no host**, após fechar o cliente, perder a VPN ou reiniciar o notebook, respeitando os requisitos de conexão e controles locais documentados. Ambos os lados precisam ser Windows, e o host deve continuar ligado e com o usuário conectado; não recupera processos após reiniciar o host.

<a id="native-setup"></a>

## Requisitos e confiança

Serve quando ambos usam Windows, os túneis usam a mesma conta GitHub e conexões de saída são permitidas. O host deve ficar ligado com o usuário conectado. Não recupera processos após reinicialização, logoff, falha ou desligamento do host, nem exit do shell. Os downloads atuais têm assinatura de desenvolvimento, não são versões de produção com confiança pública.

[Verifique o download e reveja a decisão de confiança antes de instalar](../../DEVELOPMENT-INSTALL.md). Não contorne avisos do sistema ou políticas da organização.

Cliente e host precisam usar Windows. O host deve continuar ligado e com o usuário conectado. O cliente usa Microsoft devtunnel CLI; o host, uma CLI nativa compatível de túneis do VS Code, como code-tunnel.exe. O editor completo é opcional. Ambos os túneis precisam da mesma conta do GitHub; a autenticação do gh é separada. É preciso permitir conexões de saída, consentir com downloads de dependências e aceitar a licença do VS Code server.

Escolha x64 ou ARM64 em [Downloads para Windows](https://github.com/yeelam/arterm/releases/latest). A rota verificada redireciona para v0.7.1. Os pacotes têm assinatura de desenvolvimento, não um certificado de produção com confiança pública. Siga o [guia de instalação](../../DEVELOPMENT-INSTALL.md) para conferir checksums e a impressão digital do certificado público. Só adicione confiança ao usuário atual com aprovação explícita e permissão da organização. Não contorne Authenticode, SmartScreen ou controles de aplicativos.

**Antes de adicionar confiança ou instalar:** siga a [comparação do ZIP](../../DEVELOPMENT-INSTALL.md#verify-the-downloaded-release-payload-before-trust-or-installation) com Get-FileHash e o checksum do arquivo publicado na release. A impressão digital do CER não verifica o ZIP nem os executáveis. Pare se o checksum público estiver ausente ou não corresponder; uma correspondência não garante segurança nem aprova confiança.

O host também exige Windows Script Host/VBScript permitido pela política. Confira antes de confiar ou instalar; se bloqueado, pare sem contornar a política.

## Configuração

**Mantenha os dados da sessão privados:** saída retida, histórico normal do shell e arquivos de recuperação podem conter comandos, caminhos, código ou segredos. Proteja-os nos dois computadores e remova informações sensíveis antes de compartilhar logs, capturas ou pacotes de suporte. A proteção de credenciais por DPAPI não significa que todos os arquivos sejam criptografados ou sem conteúdo. As exclusões de conteúdo dos diagnósticos de prontidão são mais restritas.

Antes de executar, use Get-AuthenticodeSignature conforme a [inspeção somente leitura](../../DEVELOPMENT-INSTALL.md#inspect-extracted-executable-signatures-without-running-them) dos instaladores e clientes extraídos. Checksum ZIP, identidade CER e confiança Windows são verificações distintas. Exija o signatário de desenvolvimento identificado pela impressão digital SHA-256 do certificado indicada no guia e status Valid; signatário desconhecido ou status não Valid exige parar, não contornar avisos. A confiança depende de aprovação explícita e política separadamente, seguida de nova inspeção. O controle é restrito ao mesmo usuário local, sessão, integridade/elevação e cliente assinado idêntico; não é compartilhamento arbitrário entre agentes de máquinas diferentes.

No host, execute arTerm-Host-Setup.exe e abra um novo PowerShell:

```powershell
arterm-host setup --name my-devbox
```

Conclua o login do túnel e guarde o comando de registro mostrado. Instale arTerm-Client-Setup.exe localmente, abra um novo PowerShell e use a mesma conta do GitHub. Execute exatamente o registro impresso pelo host com o caminho real informado. O instalador já inicializa o cliente; arterm setup não é necessário. Com um nome de sessão novo, conecte localmente:

```powershell
arterm connect my-devbox MyProof --shell powershell
```

## Comprovar o mesmo shell

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

## Limites

Não há recuperação de processos após reinicialização, logoff, falha ou desligamento do host, nem após saída do shell. O host inicia no login do usuário; não configura login automático nem serviço sem usuário conectado. A reprodução de saída tem retenção limitada. A branch de desenvolvimento inteira não está qualificada para release: a revisão independente dos arquivos de transferência e os critérios de entrada direta continuam pendentes. TUIs arbitrárias não são certificadas; desbloquear arquivos automaticamente não significa que sejam seguros.

A reprodução é limitada; guarde um log de compilação remoto para toda a saída. arterm-host stop pode reiniciar automaticamente; use arterm-host stop --disable para manter a parada.

## Recuperação e desfazer

Se falhar, execute arterm doctor my-devbox e confira contas e rede; use arterm --login para recuperar explicitamente o login do cliente quando necessário. Não apague registros de recuperação nem reinstale um host em execução. Atualizações não interrompem sessões ativas por padrão. No host, arterm-host stop --disable mantém o host parado; arterm-host start inicia novamente, mas não recupera shells perdidos. A desinstalação mantém dados; o guia explica como remover a confiança no certificado.

## Controle local avançado

send/read gerenciados exigem nova sessão PowerShell/pwsh compatível e identidade local confiável correspondente à conexão. Shells existentes não recebem a integração. A publicação não conclui os critérios de entrada direta/revisão de arquivos transferidos da branch; versões desconhecidas seguem não verificadas.

Usuários de Copilot CLI, Claude Code, Codex, Gemini CLI, Kimi e Qwen CLI podem ler e executar comandos comuns do Windows quando suas ferramentas permitirem. Isso não declara integração nativa nem certificação dos seis clientes. Outro terminal de controle local exige o mesmo usuário, sessão e contexto de integridade/elevação, além de cliente assinado confiável e idêntico byte a byte. Mantenha a conexão em execução.

[README](../../README.md) · [Quick start](../../QUICKSTART.md) · [Automation](../../QUICKSTART.md#automation) · [Solução de problemas](../../README.md#installation-and-troubleshooting-details) · [Atualizar e parar](../../QUICKSTART.md#upgrade-without-registering-again) · [Remover a confiança no certificado](../../DEVELOPMENT-INSTALL.md) · [Release evidence / gates](../../README.md#released-downloads-versus-development-gates) · [Signing](../../SIGNING.md)
