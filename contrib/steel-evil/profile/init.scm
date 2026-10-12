;; init.scm for the isolated steel-evil profile (contrib/steel-evil/hx-evil).
;;
;; "vim-hx/init.scm" resolves through $STEEL_HOME/cogs/vim-hx/init.scm,
;; which fetch-vim-hx.sh populates with mattwparas/vim.hx at a pinned SHA.
(require "helix/keymaps.scm")
(require "vim-hx/init.scm")

(set-vim-keybindings!)

;; add-global-keybinding merges into the current keymap, so TOML keys survive.
;; Re-assert the MCP keys anyway in case a future vim.hx binds F11/F12.
(keymap (global)
        (normal (F11 ":mcp") (F12 ":mcp-connect mirage"))
        (select (F11 ":mcp") (F12 ":mcp-connect mirage"))
        (insert (F11 ":mcp") (F12 ":mcp-connect mirage")))
