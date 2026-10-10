# arTerm : terminal Windows distant persistant après déconnexion

[English](../../README.md) | [简体中文](README.zh-CN.md) | [日本語](README.ja.md) | [Español](README.es.md) | [Português (Brasil)](README.pt-BR.md) | [Français](README.fr.md) | [Deutsch](README.de.md)

Perdez la connexion, pas votre travail. Laissez compilations, commandes longues et agents de programmation sur le poste Windows distant. Fermez le client, perdez le VPN ou redémarrez le portable, puis retrouvez le même shell sans repartir de zéro.

Client et hôte doivent utiliser Windows. L’hôte doit rester en marche avec l’utilisateur connecté. Le client utilise Microsoft devtunnel CLI ; l’hôte, une CLI native compatible avec les tunnels VS Code, telle que code-tunnel.exe. L’éditeur complet est facultatif. Les deux tunnels doivent utiliser le même compte GitHub ; l’authentification de gh est distincte. Les connexions sortantes doivent être autorisées. Le téléchargement des dépendances nécessite votre accord, et la configuration de l’hôte demande l’acceptation de la licence VS Code server.

Choisissez x64 ou ARM64 dans les [téléchargements Windows](https://github.com/yeelam/arterm/releases/latest). Le lien vérifié redirige vers v0.7.1. Les archives portent une signature de développement, pas un certificat de production reconnu publiquement. Suivez le [guide d’installation](../../DEVELOPMENT-INSTALL.md) pour vérifier les sommes de contrôle et l’empreinte du certificat public. N’ajoutez la confiance que pour l’utilisateur courant, avec accord explicite et si votre organisation l’autorise. Ne contournez pas Authenticode, SmartScreen ou le contrôle des applications.

Sur l’hôte, exécutez arTerm-Host-Setup.exe puis ouvrez un nouveau PowerShell :

```powershell
arterm-host setup --name my-devbox
```

Terminez l’authentification du tunnel et conservez la commande d’enregistrement affichée. Installez arTerm-Client-Setup.exe localement, ouvrez un nouveau PowerShell et utilisez le même compte GitHub. Exécutez exactement l’enregistrement fourni par l’hôte, avec son chemin réel. L’installation initialise déjà le client ; arterm setup n’est pas nécessaire. Avec un nouveau nom de session, connectez-vous localement :

```powershell
arterm connect my-devbox MyProof --shell powershell
```

Dans le PowerShell distant, exécutez ces lignes et notez le PID et le répertoire :

```powershell
$artermProof = 'kept-on-host'
$artermPid = $PID
$artermCwd = (Get-Location).Path
$PID; $artermProof; (Get-Location).Path
```

Appuyez sur Ctrl+] pour détacher le client. Si le terminal intercepte le raccourci, fermez seulement l’onglet du client local. Gardez l’hôte en marche et l’utilisateur connecté. Relancez localement la même commande :

```powershell
arterm connect my-devbox MyProof --shell powershell
```

Dans le PowerShell distant retrouvé :

```powershell
$PID; $artermProof; (Get-Location).Path
($PID -eq $artermPid) -and ($artermProof -eq 'kept-on-host') -and ((Get-Location).Path -eq $artermCwd)
```

Vous devez retrouver le même PID, kept-on-host, le même répertoire et True. Réutilisez cette commande pour reprendre votre travail lors des prochaines connexions. Pour terminer le test, saisissez exit dans le shell distant : le processus se termine et son état ne peut plus être repris.

Les processus ne sont pas restaurés après redémarrage, fermeture de session, crash ou arrêt de l’hôte, ni après sortie du shell. L’hôte démarre à la connexion de l’utilisateur, sans configurer de connexion automatique ni de service sans utilisateur connecté. La relecture de sortie est limitée. Toute la branche de développement n’est pas qualifiée pour publication : la revue indépendante des archives de transfert et les critères de saisie directe restent incomplets. Les TUI arbitraires ne sont pas certifiées ; le déblocage automatique des fichiers ne garantit pas leur sécurité.

En cas d’échec, lancez arterm doctor my-devbox et vérifiez comptes et réseau ; utilisez arterm --login pour rétablir explicitement l’authentification du client si nécessaire. Ne supprimez pas les données de récupération et ne réinstallez pas un hôte actif. Les mises à jour refusent par défaut d’interrompre les sessions actives. Sur l’hôte, arterm-host stop --disable maintient l’arrêt ; arterm-host start le relance sans ressusciter les shells perdus. La désinstallation conserve les données ; le guide explique comment retirer la confiance du certificat.

Les utilisateurs de Copilot CLI, Claude Code, Codex, Gemini CLI, Kimi et Qwen CLI peuvent lire ces instructions et lancer des commandes Windows ordinaires si leurs outils le permettent. Ce n’est pas une certification d’intégration native de ces six clients. Un autre terminal de contrôle local exige le même utilisateur, la même session et le même contexte d’intégrité/élévation, avec un client signé approuvé et identique octet par octet. Gardez le processus de connexion actif.

[README](../../README.md) · [Quick start](../../QUICKSTART.md) · [Automation](../../QUICKSTART.md#automation) · [Troubleshooting](../../README.md#installation-and-troubleshooting-details) · [Upgrade / stop](../../QUICKSTART.md#upgrade-without-registering-again) · [Trust removal](../../DEVELOPMENT-INSTALL.md) · [Release evidence / gates](../../README.md#released-downloads-versus-development-gates) · [Signing](../../SIGNING.md)
