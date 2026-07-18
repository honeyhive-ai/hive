// Self-host Monaco instead of @monaco-editor/react's default CDN loader — a
// Tauri webview has no guaranteed network at runtime. Import this once before
// rendering. Vite bundles the editor worker via the `?worker` suffix.
import * as monaco from "monaco-editor";
import editorWorker from "monaco-editor/esm/vs/editor/editor.worker?worker";
import { loader } from "@monaco-editor/react";

self.MonacoEnvironment = {
  getWorker: () => new editorWorker(),
};

loader.config({ monaco });
