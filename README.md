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

### Publier une nouvelle release

```bash
git tag v0.1.0
git push origin v0.1.0
```

La release apparaît en brouillon sur GitHub une fois les 3 builds terminés ; il suffit de la
publier manuellement après vérification.

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
