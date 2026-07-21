<div align="center">

# OneShell

**An all-in-one desktop terminal and server management studio.**

SSH · SFTP · Port Forwarding · Monitoring · AI Assistant — bundled into a single Tauri app.

[![Tauri](https://img.shields.io/badge/Tauri-2.x-FFC131?logo=tauri&logoColor=white)](https://tauri.app/)
[![Vue](https://img.shields.io/badge/Vue-3.5-42B883?logo=vue.js&logoColor=white)](https://vuejs.org/)
[![TypeScript](https://img.shields.io/badge/TypeScript-5.6-3178C6?logo=typescript&logoColor=white)](https://www.typescriptlang.org/)
[![Rust](https://img.shields.io/badge/Rust-1.83+-DEA584?logo=rust&logoColor=white)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/license-MIT-blue.svg)](./LICENSE)

</div>

---

## ✨ Features

| | |
|---|---|
| 🖥️ **Multi-tab SSH Terminal** | Real `xterm.js` sessions over `russh`, with split tabs and rich output rendering. |
| 📁 **SFTP Browser** | Browse, upload, download and edit remote files through a native file panel. |
| 🔁 **Port Forwarding** | Local / remote / dynamic SSH tunnels configurable per host. |
| 📊 **Live Monitoring** | CPU, memory, disk and network metrics streamed straight from the remote host. |
| 🤖 **AI Assistant** | Ask questions about the current session — Markdown-rendered, sanitized, context-aware. |
| 🗂️ **Host Library** | Persist host profiles locally; one-click connect. |
| ⚡ **Native & Fast** | Tauri 2 delivers a ~10 MB binary with a Rust backend and a tiny memory footprint. |

---

## 🧱 Tech Stack

**Frontend**
- [Vue 3](https://vuejs.org/) (Composition API, `<script setup>`)
- [TypeScript](https://www.typescriptlang.org/) · [Vite 6](https://vite.dev/)
- [TailwindCSS 4](https://tailwindcss.com/) · [shadcn-vue](https://www.shadcn-vue.com/) (`reka-nova` style, Geist font)
- [xterm.js](https://xtermjs.org/) · [marked](https://marked.js.org/) + [DOMPurify](https://github.com/cure53/DOMPurify) · [Reka UI](https://reka-ui.com/)

**Backend** (`src-tauri/`)
- [Rust](https://www.rust-lang.org/) · [Tauri 2](https://tauri.app/)
- [russh](https://github.com/Eugeny/russh) (pure-Rust SSH client) + `russh-sftp`
- [tokio](https://tokio.rs/) (async runtime) · [reqwest](https://github.com/seanmonstar/reqwest) (rustls, streaming)
- [serde](https://serde.rs/) · [uuid](https://github.com/uuid-rs/uuid) · [parking_lot](https://github.com/Amanieu/parking_lot)

**Tooling**
- [pnpm 10](https://pnpm.io/) · [Vue TSC](https://www.npmjs.com/package/vue-tsc)

---

## 🚀 Getting Started

### Prerequisites

- **Node.js ≥ 20** & **pnpm ≥ 10**
  ```bash
  corepack enable
  corepack prepare pnpm@latest --activate
  ```
- **Rust** (stable, ≥ 1.83) — install via [rustup](https://rustup.rs/)
- **Tauri 2 platform deps** — see the [official prerequisites](https://tauri.app/start/prerequisites/) for your OS:
  - **macOS**: Xcode Command Line Tools (`xcode-select --install`)
  - **Windows**: Microsoft Visual Studio C++ Build Tools + WebView2
  - **Linux**: `webkit2gtk`, `libayatana-appindicator3`, etc.

### Install

```bash
pnpm install
```

### Run in development

```bash
pnpm tauri dev
```

The first launch will compile the Rust backend (a few minutes); subsequent runs are incremental.

### Build a release binary

```bash
pnpm tauri build
```

Output is placed under `src-tauri/target/release/bundle/` (`.dmg` / `.msi` / `.AppImage` / `.deb`, depending on your OS).

---

## 🗂️ Project Structure

```
OneShell/
├── src/                       # Vue 3 frontend
│   ├── components/
│   │   ├── ui/                # shadcn-vue primitives (auto-managed)
│   │   ├── AiPanel.vue        # AI chat with markdown rendering
│   │   ├── AiCommandBar.vue   # Quick AI input bar
│   │   ├── AiSettingsDialog.vue
│   │   ├── HostSidebar.vue    # Saved hosts
│   │   ├── HostFormDialog.vue
│   │   ├── TerminalView.vue   # xterm.js wrapper
│   │   ├── TerminalTabs.vue
│   │   ├── SftpPanel.vue
│   │   ├── ForwardPanel.vue
│   │   ├── MonitorPanel.vue
│   │   └── SidePanel.vue      # Unified side panel host
│   ├── lib/
│   │   ├── api.ts             # Tauri command bindings
│   │   ├── store.ts           # Pinia-style local store
│   │   ├── ai-conversations.ts
│   │   ├── term-context.ts    # Terminal selection / snapshot context
│   │   └── utils.ts
│   ├── assets/main.css
│   ├── App.vue
│   └── main.ts
│
├── src-tauri/                 # Rust backend
│   ├── src/
│   │   ├── lib.rs             # Tauri app entry
│   │   ├── main.rs
│   │   ├── ssh.rs             # russh client + session pool
│   │   ├── sftp.rs            # SFTP operations
│   │   ├── forward.rs         # Port forwarding
│   │   ├── monitor.rs         # System metrics
│   │   ├── ai.rs              # AI provider commands
│   │   ├── store.rs           # Persistent host storage
│   │   └── models.rs          # Shared types
│   ├── capabilities/default.json
│   ├── icons/
│   ├── tauri.conf.json
│   ├── Cargo.toml
│   └── build.rs
│
├── public/                    # Static assets served as-is
├── components.json            # shadcn-vue config
├── vite.config.ts
├── tsconfig.json
└── package.json
```

---

## ⚙️ Configuration

### App config
Edit `src-tauri/tauri.conf.json`:
- `productName`, `identifier`, `version`
- Window size / title (`app.windows`)
- Bundle targets (`bundle.targets`)

### AI providers
Open **Settings → AI** in the app, or store credentials in the local host store. The AI module is provider-agnostic — extend `src-tauri/src/ai.rs` to plug in a new endpoint.

### Adding a shadcn-vue component
```bash
pnpm dlx shadcn-vue@latest add <component>
```

---

## 🛠️ Common Scripts

| Command | What it does |
|---|---|
| `pnpm dev` | Vite dev server only (no Tauri shell) |
| `pnpm build` | Type-check (`vue-tsc`) + Vite production build |
| `pnpm preview` | Preview the built frontend locally |
| `pnpm tauri dev` | Full Tauri app in dev mode |
| `pnpm tauri build` | Production bundle for current OS |
| `pnpm tauri build --target <triple>` | Cross-compile (requires toolchains) |

---

## 🧭 Architecture Notes

- **IPC**: Frontend talks to Rust exclusively through typed Tauri commands (`src/lib/api.ts` ↔ `#[tauri::command]` handlers).
- **Streaming**: Long-running operations (monitoring, AI tokens, file transfers) use Tauri events / `reqwest` streams rather than blocking commands.
- **Security**: AI-rendered Markdown is run through `DOMPurify` before injection; CSP is set in `tauri.conf.json`.
- **Persistence**: Host library lives in the OS app-data dir (`dirs` crate) — never in the repo.

---

## 🤝 Contributing

1. Fork & branch: `git checkout -b feat/your-thing`
2. Keep PRs focused; follow the existing module boundaries (one Rust module per concern).
3. Run before pushing:
   ```bash
   pnpm tauri build   # catches both compile errors
   ```
4. Open a PR describing the user-visible change and any new Tauri commands.

---

## 📝 License

[MIT](./LICENSE) — see `LICENSE` for details.

---

## 🙏 Acknowledgments

Built on the shoulders of:
[Tauri](https://tauri.app/) · [Vue.js](https://vuejs.org/) · [russh](https://github.com/Eugeny/russh) ·
[xterm.js](https://xtermjs.org/) · [shadcn-vue](https://www.shadcn-vue.com/) · [Tailwind CSS](https://tailwindcss.com/)