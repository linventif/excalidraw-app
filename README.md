# Excalidraw Desktop

Dépôt : [linventif/excalidraw-app](https://github.com/linventif/excalidraw-app)

Excalidraw packagé en application native, hors ligne, via [Tauri](https://tauri.app) (Rust).
Le frontend Excalidraw (React/Vite) est compilé en fichiers statiques une seule fois au build ;
aucun Node.js n'est nécessaire pour exécuter l'application finale, seulement pour la compiler.

## Structure

```
.
├── vendor/excalidraw/     # submodule git -> excalidraw/excalidraw (source upstream)
├── src-tauri/             # projet Rust/Tauri (shell natif, packaging)
├── package.json           # scripts d'orchestration (build:web, dev, build)
└── .github/workflows/     # CI multiplateforme (Windows/macOS/Linux)
```

## Prérequis

- Node.js 20+ et Corepack (`corepack enable`) pour Yarn
- Rust stable (`rustup`) + Cargo
- Dépendances système Tauri selon l'OS :
  - **Linux (Debian/Ubuntu)** :
    ```
    sudo apt install pkg-config libwebkit2gtk-4.1-dev libssl-dev \
      libgtk-3-dev librsvg2-dev libayatana-appindicator3-dev
    ```
  - **macOS** : Xcode Command Line Tools (`xcode-select --install`)
  - **Windows** : Visual Studio Build Tools (C++) + WebView2 (préinstallé sur Windows 10/11 récents)

## Installation

```bash
git clone --recurse-submodules git@github.com:linventif/excalidraw-app.git
cd excalidraw-app
npm install
```

## Développement

```bash
npm run dev
```

Lance le serveur de dev Vite d'Excalidraw (port 3000) et ouvre la fenêtre native Tauri dessus
avec hot-reload.

## Build de production (installateur natif)

```bash
npm run build
```

Compile le frontend Excalidraw en statique puis génère l'installateur natif pour l'OS courant
dans `src-tauri/target/release/bundle/` :

- **Linux** : `.deb`, `.AppImage`, `.rpm`
- **Windows** : `.msi` / `.exe` (NSIS)
- **macOS** : `.dmg` / `.app`

## CI/CD

- `.github/workflows/ci.yml` : à chaque push/PR sur `main`, build le frontend et compile le
  projet Rust (`cargo check`) sur Linux pour détecter vite les régressions.
- `.github/workflows/release.yml` : sur un tag `v*` (ou déclenchement manuel), build les 3
  plateformes (Windows/macOS/Linux) et publie une **release GitHub draft** avec tous les
  installateurs (`.deb`, `.rpm`, `.AppImage`, `.msi`, `.dmg`) attachés.
- `.github/workflows/server-ci.yml` : à chaque push/PR sur `main` touchant `server/**`, compile
  (`cargo check`), teste (`cargo test`) et lint (`cargo clippy`) le crate du serveur de
  collaboration, indépendamment du reste du projet.
- `.github/workflows/server-release.yml` : sur un tag `server-v*` (ou déclenchement manuel),
  build le binaire `excalidraw-server` pour Linux/macOS/Windows, construit et pousse une image
  Docker sur GitHub Container Registry, et publie une **release GitHub draft** avec les binaires
  attachés.

Le serveur de collaboration versionne **indépendamment** de l'app desktop : un tag `server-v*`
ne déclenche jamais `release.yml`, et un tag `v*` ne déclenche jamais `server-release.yml`.

### Publier une nouvelle release

```bash
git tag v0.1.0
git push origin v0.1.0
```

La release apparaît en brouillon sur GitHub une fois les 3 builds terminés ; il suffit de la
publier manuellement après vérification.

## Self-hosting du serveur de collaboration (optionnel)

Un crate Rust indépendant (`server/`, package `excalidraw-server` — axum + socketioxide) permet
d'activer la collaboration en temps réel (édition simultanée, curseurs) et la persistance des
scènes/fichiers, façon "Excalidraw Plus" mais auto-hébergée. **Ce serveur est strictement
optionnel** : sans configuration côté app, le mode desktop continue de fonctionner à 100%
hors-ligne, exactement comme aujourd'hui, sans aucun appel réseau.

### Lancer le serveur

```bash
cd server
docker compose up -d
```

Voir `server/docker-compose.yml` pour le service et le volume de données par défaut. Le serveur
peut aussi être compilé et lancé directement (`cargo run --release --manifest-path server/Cargo.toml`)
ou via un des binaires précompilés attachés à chaque release `server-v*` (voir plus bas), pour les
personnes qui préfèrent l'exécuter directement sur leur machine plutôt que via Docker.

### Variables d'environnement

| Variable          | Obligatoire | Description |
|-------------------|:-----------:|-------------|
| `PORT`            | non         | Port d'écoute HTTP/WebSocket du serveur. |
| `INSTANCE_TOKEN`  | **oui**     | Jeton secret partagé, **sans valeur par défaut** : le serveur doit refuser toute requête REST/connexion Socket.IO sans le header `Authorization: Bearer <INSTANCE_TOKEN>` (ou l'équivalent dans le payload `auth` du handshake). À générer soi-même (`openssl rand -hex 32`) avant tout déploiement. |
| `DATABASE_PATH`   | non         | Chemin du fichier SQLite utilisé pour la persistance des scènes et de leurs métadonnées. |
| `DATA_DIR`        | non         | Dossier de stockage des fichiers/pièces jointes chiffrés côté client. |
| `ALLOWED_ORIGINS` | non         | Origines CORS autorisées (inclure `tauri://localhost`, et `https://tauri.localhost` sur Windows, pour que l'app desktop puisse s'y connecter). |

### Reverse proxy (TLS)

Le serveur ne termine pas le TLS lui-même : pour un déploiement réel, placez-le derrière un
reverse proxy (Caddy ou Traefik) qui gère le certificat. Exemple minimal avec Caddy :

```caddyfile
collab.mondomaine.tld {
    reverse_proxy localhost:PORT
}
```

Caddy obtient et renouvelle automatiquement un certificat Let's Encrypt pour le domaine.

### Publier une nouvelle release du serveur

```bash
git tag server-v0.1.0
git push origin server-v0.1.0
```

Déclenche `.github/workflows/server-release.yml` : build des binaires Linux/macOS/Windows,
publication de l'image Docker sur `ghcr.io/linventif/excalidraw-app-server` (tag de version +
`latest`), et création d'une release GitHub draft avec les binaires attachés. La release apparaît
en brouillon une fois les jobs terminés ; il suffit de la publier manuellement après vérification.

## État actuel / prochaines étapes

- [x] Vendoring d'Excalidraw en submodule + build statique validé
- [x] Scaffold Tauri (fenêtre, icônes, packaging multi-cibles)
- [x] Plugins Rust `dialog` et `fs` déclarés (accès natif au système de fichiers)
- [ ] Intégration côté frontend : brancher l'ouverture/sauvegarde `.excalidraw` sur les APIs
      Tauri (`@tauri-apps/plugin-dialog`, `@tauri-apps/plugin-fs`) plutôt que sur l'API navigateur
      File System Access (peu fiable sous WebKitGTK/Linux)
- [ ] Désactiver/masquer les fonctionnalités réseau non pertinentes hors-ligne (collaboration
      temps réel, sync Firebase, partage de lien) dans `vendor/excalidraw/excalidraw-app`
- [ ] Icônes personnalisées (actuellement icônes par défaut Tauri, à remplacer)
- [ ] Signature de code Windows + notarization macOS pour éviter les avertissements de sécurité
      à l'installation
- [x] Build Linux validé de bout en bout : `npm run build` produit avec succès `.deb` (25 Mo),
      `.rpm` (25 Mo) et `.AppImage` (101 Mo) dans `src-tauri/target/release/bundle/`
- [ ] Lancement visuel de l'app (pas de serveur d'affichage dans ce sandbox pour tester l'UI)
- [ ] Build et test réels sur Windows et macOS
- [x] CI du serveur de collaboration (`server-ci.yml`) : `cargo check`/`test`/`clippy` sur
      `server/**` uniquement, YAML validé
- [x] Release du serveur de collaboration (`server-release.yml`) : binaires Linux/macOS/Windows +
      image Docker GHCR sur tag `server-v*`, YAML validé
- [ ] Implémentation du serveur (`server/`, crate `excalidraw-server`) : relais Socket.IO,
      persistance SQLite/fichiers, auth par `INSTANCE_TOKEN` — en cours en parallèle, voir
      section « Self-hosting » ci-dessus pour le contrat de variables d'environnement
- [ ] Intégration côté frontend du serveur optionnel (écran de réglages URL + token, bascule de
      `data/firebase.ts` vers un nouveau `data/backend.ts` branché sur le serveur auto-hébergé)
