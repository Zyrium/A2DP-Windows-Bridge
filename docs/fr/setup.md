---
title: Installation
layout: default
parent: Accueil
nav_order: 1
---

# Installation
{: .no_toc }

## Table des matières
{: .no_toc .text-delta }

1. TOC
{:toc}

---

## Prérequis

- **Windows 10 (22H2) / 11** (x64 ou ARM64)
- **Un adaptateur Bluetooth USB pour BlueCaster** : un dongle séparé est recommandé ; un adaptateur USB intégré convient aussi, mais Windows perd alors son propre Bluetooth (voir l'[étape 2](#winusb))

BlueCaster attribue lui-même le pilote WinUSB à l'adaptateur : [Zadig](https://zadig.akeo.ie/) n'est donc plus nécessaire (il fonctionne toujours, voir [Utiliser Zadig à la place](#zadig)).

## Installer BlueCaster

Chaque [version](https://github.com/SeiyaFunaokaJP/BlueCaster/releases) est proposée sous deux formes contenant les mêmes fichiers :

- **Programme d'installation** (`BlueCaster-<version>-win64-setup.exe`, sur ARM64 `-arm64-setup.exe`) : installe pour l'utilisateur actuel sans droits administrateur, ajoute une entrée au menu Démarrer (**BlueCaster**) et un programme de désinstallation, et installe le runtime Microsoft Edge WebView2 s'il est absent (Windows 11 l'inclut). Les mises à jour s'installent depuis l'application (**Paramètres > Mises à jour > Mettre à jour maintenant**). Les programmes vont dans `%LOCALAPPDATA%\BlueCaster\` (les trois programmes, le programme de désinstallation et un dossier `licenses\` contenant les textes de licence)
- **Portable** (`BlueCaster-<version>-win64-portable.zip`, sur ARM64 `-arm64-portable.zip`) : extrayez-le où vous voulez et lancez `bluecaster.exe`. Gardez les trois programmes (`bluecaster.exe`, `bluecaster-core.exe`, `bluecaster-driver.exe`) ensemble. La copie portable se contente de vous signaler les mises à jour ; elle nécessite que le runtime WebView2 soit installé

Les deux formes enregistrent les paramètres, les profils, les clés de liaison et le firmware dans `%APPDATA%\BlueCaster` : vous pouvez donc passer de l'une à l'autre. La désinstallation supprime ce dossier, sauf si vous désactivez **Supprimer les données utilisateur** (une désinstallation silencieuse le conserve).

### Mise à niveau depuis la v1 {#from-v1}

Lancez le programme d'installation par-dessus la v1. Sa page « Prêt à installer » indique les versions antérieures qu'il désinstallera d'abord (la v1, ou toute 2.0.x). Si la v1 est en cours d'exécution, il vous demande de la fermer, puis il la désinstalle en conservant le dossier des paramètres. Les profils, les associations avec vos casques et les fichiers de firmware sont repris : inutile d'associer à nouveau le casque. Un adaptateur qui a déjà WinUSB (installé avec Zadig) le conserve. Si **Démarrer avec Windows** était activé, c'est désormais la v2 qui démarre avec Windows.

À son premier démarrage, la v2 copie les paramètres de la v1 dans `%APPDATA%\BlueCaster\backup\v1\` avant de les lire.

### Mise à jour depuis la 2.0.x (A2DP Windows Bridge) {#from-2x}

BlueCaster 3.0.0 est le nouveau nom d'A2DP Windows Bridge. Lancez le programme d'installation de la 3.0.0 par-dessus la 2.0.x installée : il désinstalle automatiquement l'ancienne version et conserve vos paramètres. L'entrée du menu Démarrer et celle de **Démarrer avec Windows** sont renommées « BlueCaster ».

Au premier démarrage, la 3.0.0 renomme le dossier `%APPDATA%\A2DPWB` en `%APPDATA%\BlueCaster` et range ses fichiers dans des sous-dossiers (voir [Paramètres et fichiers](architecture#settings-and-files)). `backup\v2\` contient une copie des paramètres et des associations de la 2.0.x, au cas où vous voudriez revenir à la 2.0.2 : il suffit alors de recopier ces fichiers dans `%APPDATA%\A2DPWB`.

Le casque peut continuer à afficher l'ancien nom Bluetooth « A2DPWB » jusqu'à ce que vous l'associiez de nouveau.

## Étape 1 : configuration au premier lancement {#first-run}

Branchez l'adaptateur Bluetooth USB et lancez BlueCaster. Au premier démarrage (aucun appareil audio pour l'instant, ou aucun adaptateur avec WinUSB), BlueCaster ouvre l'**Assistant de configuration**, en quatre étapes. Une fois l'assistant terminé ou ignoré, il ne s'ouvre plus automatiquement ; ajoutez ensuite vos appareils depuis l'**Accueil** ou **Périphériques audio**.

1. **Choisir l'adaptateur** : affiche les adaptateurs Bluetooth USB détectés. Si le vôtre n'apparaît pas, branchez-le et cliquez sur **Actualiser**. Une étiquette verte **Pris en charge** ou rouge **Non pris en charge** indique si BlueCaster peut utiliser chaque adaptateur ; un adaptateur non pris en charge apparaît dans la liste, mais ne peut pas être sélectionné
2. **Pilote et firmware** : l'étape indique ce qu'elle va faire, et **Configurer cet adaptateur** s'en charge. BlueCaster remplace le pilote de l'adaptateur par WinUSB (Windows demande l'autorisation de l'administrateur, voir [Étape 2](#winusb)) et télécharge les fichiers de firmware Realtek dont l'adaptateur a besoin (voir [Firmware](#firmware)). Le firmware que BlueCaster ne peut pas télécharger doit être copié à la main dans le dossier du firmware. Si Windows a des appareils associés par cet adaptateur (par exemple un clavier ou une souris sur un adaptateur intégré), le bouton les liste d'abord : ils cessent de fonctionner dans Windows tant que BlueCaster utilise l'adaptateur, et **Annuler** permet de les garder. Les appareils dotés de leur propre récepteur USB ne sont pas concernés ; pour garder les autres, utilisez BlueCaster avec un adaptateur USB distinct. **Remplacer par le pilote WinUSB**, sur l'écran **Adaptateur**, les liste également
3. **Trouver vos appareils audio** : la recherche démarre automatiquement. Mettez l'appareil audio en mode d'association et cliquez sur **Connecter** à côté de son nom (ou saisissez son adresse et cliquez sur **Utiliser**) : BlueCaster enregistre l'appareil avec le meilleur codec qu'il prend en charge (Auto) et s'y connecte. L'appareil apparaît alors sous **Appareils associés** avec un point d'état, et la recherche s'arrête. **Ajouter un appareil** le déconnecte et relance la recherche pour un autre appareil ; le premier reste enregistré, et vous pouvez y oublier un appareil ajouté par erreur. **Importer les associations de Windows** apparaît quand Windows a associé des appareils via cet adaptateur : son bouton **Importer** les liste, tous cochés (voir [Étape 3](#headphones)). **Suivant** fonctionne aussi sans appareil ; vous pourrez ajouter des appareils audio plus tard dans **Périphériques audio**. Les nouveaux appareils capturent le **Hi-Fi Cable** s'il est installé, sinon VB-CABLE. Le Hi-Fi Cable ajoute le moins de latence : il est donc recommandé pour le son, et VB-CABLE pour le micro du casque. S'il manque un câble, l'étape affiche un avertissement, et son lien **En savoir plus** présente les deux câbles. Pour VB-CABLE, **Installer** télécharge le paquet VB-CABLE de VB-Audio (1,3 Mo) et l'installe (avec une demande d'autorisation administrateur ; si elle n'apparaît pas, redémarrez Windows). Pour le Hi-Fi Cable, **Télécharger** ouvre la page de VB-Audio. Un câble déjà installé affiche **Désinstaller** à la place. Les deux sont des donationwares de VB-Audio ([vb-cable.com](https://vb-cable.com/)), comme l'indique **À propos** : BlueCaster n'inclut pas VB-CABLE, et les dons vont à VB-Audio. Dans une entreprise ou une autre organisation, VB-Audio demande une licence par installation ([licences](https://vb-audio.com/Services/licensing.htm)). Quand BlueCaster détecte un câble pour la première fois par la suite, les appareils réglés sur **Périphérique par défaut** passent à la capture de ce câble (VB-CABLE uniquement tant que le Hi-Fi Cable n'est pas installé), et les appareils dont le micro n'a pas de sortie fonctionnelle y envoient le micro. Vous pourrez changer le codec plus tard dans **Codecs**
4. **Paramètres généraux** : le thème, **Démarrage**, **Notifications** et la recherche de mises à jour, avec les mêmes commandes que dans Paramètres, enregistrées immédiatement. La première configuration active **Démarrer avec Windows** et **Démarrer dans la zone de notification**

**Configurer plus tard** ignore la suite ; **Paramètres > Lancer la configuration** la rouvre.

## Étape 2 : attribuer WinUSB à l'adaptateur {#winusb}

BlueCaster communique directement avec un adaptateur Bluetooth USB via WinUSB, le pilote USB générique inclus dans Windows. Un **adaptateur dédié** est recommandé : votre Bluetooth intégré continue ainsi de fonctionner normalement dans Windows.

1. Ouvrez l'écran **Adaptateur** (l'étape **Pilote et firmware** de la configuration effectue les étapes 2 à 4 avec **Configurer cet adaptateur**)
2. Le **Pilote** de l'adaptateur indique **Pilote d'origine** (ou **Aucun pilote**) : le pilote installé par Windows ou le fabricant, que WinUSB remplace. Cliquez sur **Remplacer par le pilote WinUSB**
3. Lisez la confirmation et continuez : Windows demande l'autorisation de l'administrateur (UAC)
4. Le pilote indique désormais **Pilote WinUSB**

Si Windows ne peut pas charger WinUSB immédiatement (parce que l'adaptateur est en cours d'utilisation), BlueCaster vous en informe et la ligne du pilote indique **Rebranchez ou redémarrez Windows**. Débranchez l'adaptateur et rebranchez-le ; pour un adaptateur intégré, redémarrez Windows. D'ici là, la connexion échoue avec `failed to initialize WinUSB (error 50)` dans le journal.

{: .warning }
Tant que WinUSB est attribué, le Bluetooth de Windows ne voit plus l'adaptateur : les appareils associés à Windows par cet adaptateur ne fonctionnent plus jusqu'à la restauration du pilote. Utilisez votre Bluetooth intégré pour les périphériques courants (clavier, souris, etc.). Vous pouvez aussi confier à BlueCaster un adaptateur intégré qui est un périphérique USB (un Realtek sur de nombreux portables), mais le PC n'a alors **plus du tout de Bluetooth Windows** (paramètres Bluetooth, souris, claviers, autres appareils audio) jusqu'à la restauration de son pilote.

**Restaurer le pilote d'origine**, sur le même écran, rend l'adaptateur à Windows : Windows choisit de nouveau son propre pilote (normalement son pilote Bluetooth) et l'adaptateur fonctionne avec le Bluetooth de Windows. BlueCaster ne peut plus diffuser par cet adaptateur tant que WinUSB ne lui est pas de nouveau attribué. Windows n'utilise qu'un adaptateur Bluetooth à la fois. Si un autre adaptateur est actif, BlueCaster vous le signale après la restauration ; pour utiliser celui-ci dans Windows, désactivez l'autre dans le Gestionnaire de périphériques, puis débranchez et rebranchez celui-ci. Les casques que BlueCaster a associés de nouveau ont une nouvelle clé. Pour les casques que Windows avait associés par cet adaptateur, la restauration transmet cette nouvelle clé à Windows : ils s'y connectent donc sans nouvelle association. **Limitation :** seules les associations classiques sont transmises, pas les clés LE. Un appareil que Windows utilise en LE (LE Audio) doit être associé de nouveau dans Windows (le mode développeur permet d'essayer de transmettre les clés LE : **Développeur > Débogage**). Le programme de désinstallation propose la même option : une page de l'assistant de désinstallation comporte **Rétablir le pilote Bluetooth d'origine** (quand BlueCaster a attribué WinUSB à des adaptateurs ; une seule demande de droits d'administrateur) et **Supprimer les données utilisateur** (paramètres, codecs, associations et firmware dans `%APPDATA%\BlueCaster`, ainsi que ce que la fenêtre mémorise, comme **Afficher uniquement les codecs pris en charge**), toutes deux activées par défaut. Si les seuls adaptateurs sous WinUSB ont été configurés avec Zadig ou la v1 (BlueCaster n'en a donc aucune trace), la première option est affichée mais désactivée ; si vous l'activez, Windows choisit leur pilote. Les mises à jour conservent WinUSB, de même qu'une désinstallation silencieuse (`/S`). La même page propose **Supprimer VB-CABLE** et **Supprimer Hi-Fi Cable** lorsqu'ils sont installés. Ces options sont désactivées par défaut, car d'autres applications peuvent utiliser ces câbles. Ils sont supprimés sans la fenêtre d'installation de VB-Audio, après une demande de droits d'administrateur de Windows. Windows recherche ensuite les modifications matérielles pour que le câble disparaisse de la liste Son ; un redémarrage n'est proposé que s'il y figure encore.

### Utiliser Zadig à la place {#zadig}

Un pilote WinUSB installé avec [Zadig](https://zadig.akeo.ie/) fonctionne tout aussi bien :

1. Ouvrez Zadig et allez dans **Options > List All Devices**
2. Sélectionnez votre adaptateur Bluetooth USB dans la liste déroulante
3. Sélectionnez **WinUSB** comme pilote cible et cliquez sur **Replace Driver**

### À la main dans le Gestionnaire de périphériques {#device-manager}

Si l'adaptateur n'apparaît pas dans la liste de BlueCaster, si un bouton de l'écran **Adaptateur** échoue ou si votre adaptateur se comporte autrement que les autres de son type, vous pouvez faire la même chose à la main dans le Gestionnaire de périphériques de Windows (clic droit sur **Démarrer** > **Gestionnaire de périphériques**). Ces deux opérations demandent des droits d'administrateur. Les noms des menus peuvent varier légèrement selon la version de Windows.

**Remplacer par le pilote WinUSB**

1. Trouvez l'adaptateur : normalement sous **Bluetooth** avec son propre nom (par exemple « TP-Link Bluetooth USB Adapter »), ou avec une marque jaune quand son pilote n'a pas pu démarrer. **Affichage > Périphériques par connexion** aide à le distinguer du Bluetooth intégré du PC ; ne touchez pas à ce dernier, sauf si vous voulez le confier à BlueCaster
2. Clic droit > **Mettre à jour le pilote** > **Parcourir mon poste de travail pour rechercher des pilotes** > **Choisir parmi une liste de pilotes disponibles sur mon ordinateur**
3. Si Windows demande un type de périphérique, choisissez **Périphériques USB**. Décochez **Afficher les matériels compatibles**, choisissez le fabricant **Périphérique WinUsb** et le modèle **Périphérique WinUsb**, cliquez sur **Suivant** et confirmez l'avertissement
4. L'adaptateur apparaît maintenant comme **Périphérique WinUsb** sous **Périphériques USB**. Redémarrez BlueCaster (ou rouvrez l'écran **Adaptateur**) : il utilise l'adaptateur exactement comme après Zadig

**Restaurer le pilote d'origine**

1. Sous **Périphériques USB**, clic droit sur l'adaptateur (**Périphérique WinUsb**) > **Désinstaller l'appareil**. Si le pilote WinUSB vient de Zadig, cochez aussi **Tenter de supprimer le pilote de ce périphérique**, sinon Windows choisit de nouveau le pilote de Zadig
2. Débranchez l'adaptateur et rebranchez-le (ou **Action > Rechercher les modifications sur le matériel**). Windows installe son propre pilote et l'adaptateur apparaît sous **Bluetooth**
3. Si l'adaptateur y porte plutôt une marque jaune (son pilote n'a pas pu démarrer, par exemple code 31), le pilote du fabricant ne fonctionne pas avec lui : clic droit > **Mettre à jour le pilote** > **Parcourir mon poste de travail pour rechercher des pilotes** > **Choisir parmi une liste de pilotes disponibles sur mon ordinateur** > **Adaptateur Bluetooth générique**, ou attribuez de nouveau WinUSB comme ci-dessus pour l'utiliser avec BlueCaster

## Étape 3 : trouver votre casque {#headphones}

**Rechercher** (dans **Périphériques audio**) cherche les appareils à proximité par l'adaptateur USB pendant 30 secondes et les affiche au fur et à mesure qu'ils répondent. Dans la configuration, la recherche démarre automatiquement et continue jusqu'à ce qu'un appareil se connecte (**Ajouter un appareil** le déconnecte et reprend la recherche), que vous cliquiez sur **Arrêter la recherche** ou que vous fermiez la configuration. Mettez d'abord le casque en mode d'association. Les appareils déjà associés à BlueCaster sont marqués **Associé**.

Si la recherche ne trouve pas le casque, saisissez son adresse Bluetooth sous **Manuel :** et cliquez sur **Utiliser**. BlueCaster met en forme les adresses saisies ou collées au fur et à mesure : `AA:BB:CC:DD:EE:FF`, `aa-bb-cc-dd-ee-ff`, `AABBCCDDEEFF` et `AA BB CC DD EE FF` deviennent toutes `AA:BB:CC:DD:EE:FF`.

Les casques que Windows avait associés par cet adaptateur avant qu'il passe sous WinUSB peuvent garder cette association. **Importer les associations de Windows** (à l'étape des appareils de la configuration, quand de tels appareils existent) les liste, tous cochés, et importe ceux qui sont cochés : ils se connectent alors sans passer par le mode d'association. Windows demande l'autorisation de l'administrateur. Un tel appareil n'est généralement pas en mode d'association : il n'apparaît donc pas dans la recherche. S'il apparaît quand même, ou si vous saisissez son adresse, **Connecter** ou **Ajouter** vous avertit d'abord que Windows l'a associé ; **Ajouter quand même** l'associe de nouveau comme un nouvel appareil.

**Depuis les Paramètres Windows** (si le casque a été associé avec le Bluetooth intégré du PC) :
1. Ouvrez **Paramètres > Bluetooth et appareils**
2. Cliquez sur votre appareil audio
3. Cliquez sur **Propriétés**
4. L'adresse Bluetooth s'affiche (format : `AA:BB:CC:DD:EE:FF`)

**Depuis la ligne de commande** (quittez d'abord BlueCaster, zone de notification > **Quitter**) :
```
bluecaster-core.exe --cli --scan
```

Le casque démarre avec **Auto** comme codec (**Codecs**) et l'audio du système comme source de sortie (**Périphériques audio**) ; ces valeurs par défaut donnent le meilleur son. Voir [Utilisation](usage) pour tout le reste.

---

## Firmware (adaptateurs Realtek) {#firmware}

Les adaptateurs Bluetooth USB à puce Realtek (p. ex. TP-Link UB500, dongles RTL8761BU) ont besoin d'un firmware propriétaire pour fonctionner. Les adaptateurs CSR n'ont besoin d'aucun firmware. Les adaptateurs Intel, MediaTek MT79xx, Qualcomm et Broadcom peuvent nécessiter leurs propres fichiers de firmware : voir [Adaptateurs recommandés](#adapters).

### Dans l'application (téléchargement guidé)

L'écran **Adaptateur** (et l'étape **Pilote et firmware** de la configuration) indique l'état du firmware de l'adaptateur : **Firmware OK**, **Aucun fichier Realtek requis** (un adaptateur que BlueCaster ne reconnaît pas, avec la puce sur **Pas Realtek**), ou **Firmware introuvable** avec les noms des fichiers requis. Le bouton **Ouvrir la page de téléchargement** ouvre la page [linux-firmware/rtl_bt](https://git.kernel.org/pub/scm/linux/kernel/git/firmware/linux-firmware.git/tree/rtl_bt) dans votre navigateur, puis **Ouvrir le dossier du firmware** ouvre le dossier de destination. Téléchargez les fichiers `.bin` nécessaires et placez-les dans le dossier du firmware.

Les adaptateurs d'autres fabricants n'ont pas de choix **Puce Bluetooth** ; une étiquette indique à la place d'où vient le firmware : **Du pilote de MediaTek** ou **Du pilote de Qualcomm** (lu dans le pilote Windows du fabricant, qui reste installé après WinUSB), **Du pilote Wi-Fi** (Marvell ; laissez le Wi-Fi activé), **Du dossier du firmware** (Intel, avec **Ouvrir la page de téléchargement**, **Ouvrir le dossier du firmware** et **Guide de configuration**), **Facultatif** (le `.hcd` de Broadcom) ou **Inutile** (CSR, Barrot, Actions). Une fois que BlueCaster a chargé le firmware sous WinUSB, l'étiquette indique **Chargé**, ou **Non chargé** si le chargement a échoué, accompagnée de **Ouvrir la page de téléchargement**, **Ouvrir le dossier du firmware** et d'une ligne qui indique d'où vient le firmware.

Un adaptateur RTL8761B (comme le TP-Link UB500) n'a besoin d'aucun téléchargement si le pilote Windows de Realtek est installé sur le PC. BlueCaster utilise alors le firmware de ce pilote, qui reste installé après le passage de l'adaptateur à WinUSB. Pour charger un autre firmware, comme celui de Linux, sélectionnez **Utiliser un firmware personnalisé** dans le cadre Firmware de l'adaptateur, sur l'écran **Adaptateur**. BlueCaster charge alors les fichiers du dossier du firmware. Pour revenir en arrière, sélectionnez **Utiliser le firmware du pilote**.

Pour les puces de la liste **Puce Bluetooth**, **Télécharger le firmware** fait tout cela pour vous : après confirmation, BlueCaster télécharge les fichiers depuis linux-firmware (kernel.org), vérifie le SHA-256 attendu de chacun et les enregistre dans le dossier du firmware. Si linux-firmware n'a pas de fichier de configuration pour une puce, BlueCaster enregistre un `_config.bin` vide, et la puce utilise ses propres réglages. Les fichiers ne font pas partie de BlueCaster ; ils ne sont téléchargés que si vous le demandez.

BlueCaster détecte la puce de l'adaptateur et indique quels fichiers de firmware sont nécessaires.

Sans le firmware, un adaptateur Realtek s'ouvre quand même et répond avec son code en ROM : il semble fonctionner, mais la connexion ou la diffusion échoue. BlueCaster indique alors que le firmware n'est pas chargé. Un adaptateur garde le firmware chargé lors d'une session précédente tant qu'il reste alimenté : même après la suppression des fichiers, il peut donc continuer à fonctionner jusqu'au redémarrage du PC ou au débranchement de l'adaptateur.

### Réglage Puce Bluetooth

La liste **Puce Bluetooth** de l'écran **Adaptateur** détermine si BlueCaster effectue l'initialisation Realtek (envoi du firmware) sur l'adaptateur :

| Choix | Signification |
|:-------|:--------|
| **Auto** (par défaut) | Choisi d'après l'adaptateur détecté, à chaque démarrage de BlueCaster. Un adaptateur Realtek connu reçoit sa puce ; les autres adaptateurs ne reçoivent aucune initialisation Realtek |
| **Pas Realtek** | N'effectue jamais l'initialisation Realtek. À utiliser pour les adaptateurs Intel, Broadcom, CSR et autres si Auto ne reconnaît pas le vôtre |
| **RTL8761BU**, **RTL8822CU**, ... | Cette puce Realtek. Nécessaire pour un adaptateur à puce Realtek qu'Auto ne reconnaît pas (un produit OEM avec son propre VID:PID). La ligne d'état indique si ses fichiers `.bin` sont présents dans le dossier du firmware |
| **Custom** | Utilise `custom_fw.bin` et `custom_config.bin` du dossier du firmware |

Ne choisissez pas une puce Realtek ni Custom pour un adaptateur non Realtek : BlueCaster essaierait d'y envoyer un firmware Realtek, et l'adaptateur pourrait ne pas démarrer.

### Téléchargement manuel

1. Téléchargez les fichiers `.bin` de firmware et de configuration de votre puce depuis [linux-firmware/rtl_bt](https://git.kernel.org/pub/scm/linux/kernel/git/firmware/linux-firmware.git/tree/rtl_bt) (p. ex. `rtl8761bu_fw.bin` et `rtl8761bu_config.bin`)
2. Placez les deux fichiers dans le dossier du firmware de BlueCaster (`%APPDATA%\BlueCaster\firmware`, également accessible via **Ouvrir le dossier du firmware**)

{: .note }
Ces fichiers de firmware sont des binaires propriétaires de Realtek distribués via le projet linux-firmware. Ils ne sont pas inclus dans ce dépôt. Voir [WHENCE](https://git.kernel.org/pub/scm/linux/kernel/git/firmware/linux-firmware.git/tree/WHENCE) pour les conditions de redistribution.

---

## Adaptateurs recommandés {#adapters}

Pour les adaptateurs et casques réellement utilisés avec BlueCaster, avec les codecs et les rapports, voir [Matériel vérifié](hardware).

| Puce | Exemples de produits | Remarques |
|:--------|:-----------------|:------|
| Realtek | TP-Link UB500, RTL8761BU | Utilise le firmware du pilote Windows de Realtek s'il est installé ; sinon, téléchargez le firmware (voir ci-dessus). Le mieux testé |
| Realtek | RTL8852BD (`0BDA:B853`), RTL8852BT, RTL8922A | BlueCaster lit le firmware (p. ex. `rtl8852bd_mp_chip_new.dat`) dans le pilote Bluetooth Windows de Realtek : gardez donc ce pilote installé, ou placez dans le dossier du firmware le fichier indiqué dans le journal. Pas encore testé sur le matériel |
| CSR | Dongles CSR8510 génériques | Fonctionne d'emblée, sans firmware |
| Barrot | Dongles Bluetooth 5.4 / 6.0 (`33FA:0010`, `33FA:0012`) | Fonctionne avec son firmware en ROM, aucun fichier de firmware nécessaire. Un seul retour d'utilisateur pour l'instant |
| Marvell | Intégré aux Microsoft Surface Pro / Surface Book (88W8897) | Aucun fichier firmware : le pilote Wi-Fi charge aussi le firmware Bluetooth, laissez donc le Wi-Fi activé. Pas encore testé sur le matériel |
| Actions | Dongles ATS2851 (`10D7:B012`) | Fonctionne avec son firmware en ROM, aucun fichier de firmware nécessaire. L'adaptateur annonce des commandes qu'il ne prend pas en charge ; BlueCaster les masque. Pas encore testé sur le matériel |
| Broadcom | ASUS BT400 (BCM20702), autres dongles et modules d'ordinateurs portables BCM20702 / BCM20703 / BCM43142 | Les adaptateurs qui annoncent la classe constructeur `FF/01/01` au lieu de la classe Bluetooth `E0/01/01` (dongles OEM BCM20702) sont aussi détectés. Fonctionne avec le firmware en ROM. Si le pilote Bluetooth Windows de Broadcom est installé, BlueCaster envoie au démarrage le correctif de ce pilote. Si un fichier PatchRAM `.hcd` (p. ex. `BCM20702A1-0b05-17cb.hcd`) se trouve dans le dossier du firmware, BlueCaster utilise ce fichier à la place. Si le correctif du pilote échoue, BlueCaster vous demande de débrancher l'adaptateur puis de le rebrancher, et n'envoie plus le correctif à cet adaptateur. Le correctif du pilote n'est pas encore testé sur le matériel |
| Intel | Intel 7260 / 3160 / 7265 / 3165 / 3168 / 8260 / 8265 / 9260 / AX200 / AX201 / AX210 / AX211 | En mode bootloader (p. ex. après un démarrage à froid), nécessite les fichiers `ibt-*.sfi` + `ibt-*.ddc` correspondants, issus de `intel/` dans linux-firmware, dans le dossier du firmware ; le journal indique lesquels (p. ex. `ibt-0180-0041.sfi`). Les 7260 / 3160 / 7265 / 3165 / 3168 fonctionnent depuis leur ROM sans fichier ; le correctif `ibt-hw-*.bseq` correspondant est appliqué s'il est dans le même dossier |
| MediaTek | MT7925, MT7927, MT7922, MT7921, MT7902 (cartes Wi-Fi comme l'AMD RZ717, RZ738, RZ616, RZ608) | Nécessite le `BT_RAM_CODE_MT*_hdr.bin` de la puce (p. ex. `BT_RAM_CODE_MT7925_1_1_hdr.bin`). BlueCaster le lit dans le pilote Bluetooth Windows de MediaTek si ce pilote est installé. Sinon, placez dans le dossier du firmware le fichier indiqué dans le journal, issu du dossier `mediatek/` de linux-firmware. Le MT7927 n'est pas encore testé sur le matériel |
| Qualcomm | QCA61x4, QCA9377 (ROME), WCN6855 (FastConnect 6900), WCN785x (FastConnect 7800) | Nécessite le rampatch et la NVM de la puce (p. ex. `AthrBT_0x00000302.dfu` et `ramps_0x00000302_48.dfu`). BlueCaster les lit dans le pilote Bluetooth Windows de Qualcomm si ce pilote est installé. Sinon, placez dans le dossier du firmware les fichiers indiqués dans le journal, issus du dossier `qca/` de linux-firmware (`rampatch_usb_*.bin`, `nvm_usb_*.bin`). Pas encore testé sur le matériel |
| MediaTek | MT7663 | Nécessite le fichier de correctif de la puce (p. ex. `mt7663_patch_e2_hdr.bin`). BlueCaster le lit dans le pilote Bluetooth Windows de MediaTek si ce pilote est installé. Sinon, placez dans le dossier du firmware le fichier indiqué dans le journal. Pas encore testé sur le matériel |
| Qualcomm Atheros | AR3012 et autres puces AR30xx (Bluetooth 4.0 des anciens ordinateurs portables) | Nécessite les fichiers de correctif et de configuration de la puce (p. ex. `AthrBT_0x31010000.dfu` et `ramps_0x31010000_40.dfu`). BlueCaster les lit dans le pilote Bluetooth Windows de Qualcomm Atheros si ce pilote est installé. Sinon, placez dans le dossier du firmware les fichiers indiqués dans le journal. Pas encore testé sur le matériel |
| MediaTek, Qualcomm | Puces MediaTek plus anciennes (comme le MT7630 et le MT7650), autres puces Qualcomm (QCA6290) | Non pris en charge : ces puces nécessitent des chargeurs de firmware que BlueCaster n'a pas ([aider à en ajouter](chip-support)) |

Les adaptateurs Bluetooth USB 5.0+ donnent généralement les meilleurs résultats avec les codecs à haut débit comme LDAC.

Lorsque plusieurs adaptateurs ont WinUSB, BlueCaster utilise le premier qu'il trouve, sauf si vous cliquez sur **Utiliser cet adaptateur** sur la carte d'un autre, sur l'écran **Adaptateurs**. L'adaptateur utilisé est marqué **Utilisé**.
