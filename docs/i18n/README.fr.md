# arTerm : terminal Windows distant persistant après déconnexion

[English](../../README.md) | [简体中文](README.zh-CN.md) | [繁體中文](README.zh-TW.md) | [日本語](README.ja.md) | [한국어](README.ko.md) | [Español](README.es.md) | [Português (Brasil)](README.pt-BR.md) | [Français](README.fr.md) | [Deutsch](README.de.md) | [Italiano](README.it.md) | [Русский](README.ru.md) | [Türkçe](README.tr.md) | [Tiếng Việt](README.vi.md) | [Bahasa Indonesia](README.id.md) | [हिन्दी](README.hi.md) | [العربية](README.ar.md)

Perdez la connexion, pas votre travail. Laissez compilations, commandes de longue durée et agents de programmation sur le poste Windows distant. Fermez le client, perdez le VPN ou redémarrez le portable, puis retrouvez le même shell sans repartir de zéro.

L’hôte distant garde le shell en cours d’exécution ; votre portable ne fait que s’y connecter.

Convient si les deux postes utilisent Windows, les tunnels le même compte GitHub et les connexions sortantes sont autorisées. L’hôte doit rester en marche avec l’utilisateur connecté. Aucun processus n’est restauré après redémarrage, fermeture de session, crash ou arrêt de l’hôte, ni exit du shell. Les téléchargements actuels portent une signature de développement, pas une signature de production reconnue publiquement.

[Vérifiez le téléchargement et examinez la décision de confiance avant installation](../../DEVELOPMENT-INSTALL.md). Ne contournez pas les avertissements système ou les politiques de votre organisation.

[Télécharger x64 / ARM64](https://github.com/yeelam/arterm/releases/latest) · [Configurer](../../README.md#get-connected) → [Vérifier la reconnexion au même shell](../../README.md#prove-that-you-returned-to-the-same-shell)

<a href="https://github.com/yeelam/arterm/releases/latest"><img src="../../.github/arterm-before-after.png" width="700" alt="Avant et après : après fermeture du client, perte du VPN ou redémarrage du portable, retrouvez le même shell distant, ses variables et le travail ; l’hôte reste en marche avec l’utilisateur connecté."></a>

*Illustration du flux, pas une capture de session réelle ni une preuve de test. Les libellés de l’image sont en anglais. Le principe : après une déconnexion du client, retrouvez le même shell, les variables et le travail sur l’hôte qui reste en marche avec l’utilisateur connecté. Les processus ne survivent pas au redémarrage de l’hôte.*

## Prérequis et confiance

Client et hôte doivent utiliser Windows. L’hôte doit rester en marche avec l’utilisateur connecté. Le client utilise Microsoft devtunnel CLI ; l’hôte, une CLI native compatible avec les tunnels VS Code, telle que code-tunnel.exe. L’éditeur complet est facultatif. Les deux tunnels doivent utiliser le même compte GitHub ; l’authentification de gh est distincte. Les connexions sortantes doivent être autorisées. Le téléchargement des dépendances nécessite votre accord, et la configuration de l’hôte demande l’acceptation de la licence VS Code server.

Choisissez x64 ou ARM64 dans les [téléchargements Windows](https://github.com/yeelam/arterm/releases/latest). Le lien vérifié redirige vers v0.7.1. Les archives portent une signature de développement, pas un certificat de production reconnu publiquement. Suivez le [guide d’installation](../../DEVELOPMENT-INSTALL.md) pour vérifier les sommes de contrôle et l’empreinte du certificat public. N’ajoutez la confiance que pour l’utilisateur courant, avec accord explicite et si votre organisation l’autorise. Ne contournez pas Authenticode, SmartScreen ou le contrôle des applications.

**Avant d’ajouter la confiance ou d’installer :** suivez la [comparaison du ZIP](../../DEVELOPMENT-INSTALL.md#verify-the-downloaded-release-payload-before-trust-or-installation) avec Get-FileHash et la somme d’archive publiée dans la version. L’empreinte CER ne vérifie ni le ZIP ni les exécutables. Arrêtez-vous si la somme publique manque ou diffère ; une correspondance ne garantit pas la sécurité et n’autorise pas la confiance.

## Configuration

**Gardez les données de session privées :** sortie conservée, historique normal du shell et fichiers de récupération peuvent contenir commandes, chemins, code ou secrets. Protégez-les sur les deux machines et masquez les informations sensibles avant de partager journaux, captures ou dossiers de support. La protection DPAPI des identifiants ne signifie pas que tous les fichiers sont chiffrés ou sans contenu. Les exclusions de contenu des diagnostics de disponibilité sont plus limitées.

Avant toute exécution, utilisez Get-AuthenticodeSignature selon l’[inspection en lecture seule](../../DEVELOPMENT-INSTALL.md#inspect-extracted-executable-signatures-without-running-them) des installateurs et clients extraits. Somme ZIP, identité CER et confiance Windows sont distinctes. Exigez le signataire de développement fixé et le statut Valid ; un signataire inconnu ou un statut non Valid impose l’arrêt, pas le contournement des avertissements. La confiance exige séparément un accord explicite et l’autorisation de la politique, puis une nouvelle inspection. Le contrôle reste limité au même utilisateur local, à la même session, au même contexte d’intégrité/élévation et au client signé identique ; pas de partage arbitraire d’agents entre machines.

Sur l’hôte, exécutez arTerm-Host-Setup.exe puis ouvrez un nouveau PowerShell :

```powershell
arterm-host setup --name my-devbox
```

Terminez l’authentification du tunnel et conservez la commande d’enregistrement affichée. Installez arTerm-Client-Setup.exe localement, ouvrez un nouveau PowerShell et utilisez le même compte GitHub. Exécutez exactement l’enregistrement fourni par l’hôte, avec son chemin réel. L’installation initialise déjà le client ; arterm setup n’est pas nécessaire. Avec un nouveau nom de session, connectez-vous localement :

```powershell
arterm connect my-devbox MyProof --shell powershell
```

## Vérifier le même shell

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

## Limites

Les processus ne sont pas restaurés après redémarrage, fermeture de session, crash ou arrêt de l’hôte, ni après sortie du shell. L’hôte démarre à la connexion de l’utilisateur, sans configurer de connexion automatique ni de service sans utilisateur connecté. La relecture de sortie est limitée. Toute la branche de développement n’est pas qualifiée pour publication : la revue indépendante des archives de transfert et les critères de saisie directe restent incomplets. Les TUI arbitraires ne sont pas certifiées ; le déblocage automatique des fichiers ne garantit pas leur sécurité.

## Récupération et retour arrière

En cas d’échec, lancez arterm doctor my-devbox et vérifiez comptes et réseau ; utilisez arterm --login pour rétablir explicitement l’authentification du client si nécessaire. Ne supprimez pas les données de récupération et ne réinstallez pas un hôte actif. Les mises à jour refusent par défaut d’interrompre les sessions actives. Sur l’hôte, arterm-host stop --disable maintient l’arrêt ; arterm-host start le relance sans ressusciter les shells perdus. La désinstallation conserve les données ; le guide explique comment retirer la confiance du certificat.

## Contrôle local avancé

Les utilisateurs de Copilot CLI, Claude Code, Codex, Gemini CLI, Kimi et Qwen CLI peuvent lire ces instructions et lancer des commandes Windows ordinaires si leurs outils le permettent. Ce n’est pas une certification d’intégration native de ces six clients. Un autre terminal de contrôle local exige le même utilisateur, la même session et le même contexte d’intégrité/élévation, avec un client signé approuvé et identique octet par octet. Gardez le processus de connexion actif.

[README](../../README.md) · [Quick start](../../QUICKSTART.md) · [Automation](../../QUICKSTART.md#automation) · [Dépannage](../../README.md#installation-and-troubleshooting-details) · [Mise à jour et arrêt](../../QUICKSTART.md#upgrade-without-registering-again) · [Retirer la confiance du certificat](../../DEVELOPMENT-INSTALL.md) · [Release evidence / gates](../../README.md#released-downloads-versus-development-gates) · [Signing](../../SIGNING.md)
