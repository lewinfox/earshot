// Left-hand folder tree, like Rekordbox's Explorer: "All tracks" at the top, then each
// library folder, whose subfolders load from disk when you expand them.

import { invoke } from "./api";
import type { Subdir } from "./types";

const esc = (s: string) => s.replace(/[&<>"]/g, (c) => `&#${c.charCodeAt(0)};`);

export class FolderTree {
  /** null = all tracks. */
  selected: string | null = null;
  private expanded = new Set<string>();

  constructor(
    private el: HTMLElement,
    private onSelect: (path: string | null) => void,
    private onContext: (path: string, isRoot: boolean, e: MouseEvent) => void,
  ) {}

  setRoots(roots: string[]) {
    this.el.innerHTML = "";
    this.el.append(this.row(null, "All tracks", 0, false, false, "♫"));
    const head = document.createElement("div");
    head.className = "tree-head";
    head.textContent = "Explorer";
    this.el.append(head);
    if (!roots.length) {
      const empty = document.createElement("div");
      empty.className = "tree-empty muted";
      empty.textContent = "No folders yet. Menu → Import → Folder…";
      this.el.append(empty);
    }
    for (const r of roots) this.el.append(this.node({ name: r.split("/").pop() || r, path: r, has_children: true }, 0, true));
    if (this.selected != null && !roots.some((r) => this.selected === r || this.selected!.startsWith(`${r}/`))) {
      this.select(null);
    } else this.paintSelection();
  }

  select(path: string | null) {
    this.selected = path;
    this.paintSelection();
    this.onSelect(path);
  }

  private paintSelection() {
    this.el.querySelectorAll<HTMLElement>(".tree-row").forEach((r) => r.classList.toggle("sel", (r.dataset.path ?? null) === this.selected));
  }

  private node(d: Subdir, depth: number, isRoot: boolean): HTMLElement {
    const wrap = document.createElement("div");
    const row = this.row(d.path, d.name, depth, d.has_children, isRoot, "▤");
    row.title = d.path;
    const kids = document.createElement("div");
    kids.className = "tree-kids";
    wrap.append(row, kids);
    const toggle = async (open: boolean) => {
      row.classList.toggle("open", open);
      kids.innerHTML = "";
      if (!open) {
        this.expanded.delete(d.path);
        return;
      }
      this.expanded.add(d.path);
      try {
        const subs = await invoke<Subdir[]>("list_subdirs", { path: d.path });
        kids.innerHTML = "";
        for (const s of subs) kids.append(this.node(s, depth + 1, false));
        if (!subs.length) row.classList.add("leaf");
        this.paintSelection();
      } catch (e) {
        kids.innerHTML = `<div class="tree-empty muted" style="padding-left:${(depth + 2) * 14}px">${esc(String(e))}</div>`;
      }
    };
    row.querySelector(".arrow")!.addEventListener("click", (e) => {
      e.stopPropagation();
      void toggle(!row.classList.contains("open"));
    });
    row.addEventListener("dblclick", () => void toggle(!row.classList.contains("open")));
    row.addEventListener("contextmenu", (e) => {
      e.preventDefault();
      this.onContext(d.path, isRoot, e);
    });
    if (this.expanded.has(d.path)) void toggle(true);
    return wrap;
  }

  private row(path: string | null, name: string, depth: number, hasKids: boolean, isRoot: boolean, icon: string) {
    const row = document.createElement("div");
    row.className = `tree-row${hasKids ? "" : " leaf"}${isRoot ? " root" : ""}`;
    if (path != null) row.dataset.path = path;
    row.style.paddingLeft = `${6 + depth * 14}px`;
    row.innerHTML = `<span class="arrow">▸</span><span class="icon">${icon}</span><span class="name">${esc(name)}</span>`;
    row.addEventListener("click", () => this.select(path));
    return row;
  }
}

// ---- popup menus ------------------------------------------------------------------------

export interface MenuItem {
  label: string;
  action?: () => void;
  submenu?: MenuItem[];
  disabled?: boolean;
}

let open: HTMLElement[] = [];

export function closeMenus() {
  for (const m of open) m.remove();
  open = [];
}

/** Show a menu at (x, y). Submenus open to the right on hover. */
export function showMenu(x: number, y: number, items: MenuItem[], level = 0) {
  if (level === 0) closeMenus();
  else {
    for (const m of open.splice(level)) m.remove();
  }
  const m = document.createElement("div");
  m.className = "menu-pop";
  for (const it of items) {
    const b = document.createElement("div");
    b.className = `menu-item${it.disabled ? " disabled" : ""}${it.submenu ? " sub" : ""}`;
    b.textContent = it.label;
    b.addEventListener("mouseenter", () => {
      if (it.submenu) {
        const r = b.getBoundingClientRect();
        showMenu(r.right - 2, r.top - 4, it.submenu, level + 1);
      } else {
        for (const sub of open.splice(level + 1)) sub.remove();
      }
    });
    b.addEventListener("click", (e) => {
      e.stopPropagation();
      if (it.disabled || it.submenu) return;
      closeMenus();
      it.action?.();
    });
    m.append(b);
  }
  document.body.append(m);
  const r = m.getBoundingClientRect();
  m.style.left = `${Math.max(0, Math.min(x, innerWidth - r.width - 4))}px`;
  m.style.top = `${Math.max(0, Math.min(y, innerHeight - r.height - 4))}px`;
  open.push(m);
}

window.addEventListener("mousedown", (e) => {
  if (!(e.target as HTMLElement).closest?.(".menu-pop, #menu-btn")) closeMenus();
});
window.addEventListener("keydown", (e) => e.key === "Escape" && closeMenus());
