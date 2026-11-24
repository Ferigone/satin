# Satin DevTools

Press **F12** or **Ctrl+Shift+I** to open the dock. Drag its top edge to give it more room. Click a tab or, while the dock has focus, press **Alt+1** through **Alt+6**. The panels follow the active browser tab. A new top-level navigation starts a new inspection session.

## Inspect a page and change its CSS

1. Press **Ctrl+Shift+C** or click **Pick element**. Move the pointer over the page to highlight a box. Click it to select its DOM node without activating the link, button or control. Press **Escape** to cancel.
2. In **Elements**, expand the tree, or press **Ctrl+F** to find a tag, ID, class or text. The **Node** pane shows the element's ancestors, attributes, text and box geometry. **Rules** shows directly matching author rules, declarations, specificity, source order and inline styles. **Computed** reads the selected node's resolved properties from Satin's layout engine.
3. Press **F2** to edit an inline declaration, or click a property in **Computed** to start from that property. Enter a property name and value, then choose **Apply**. **Remove** deletes the inline declaration. The page’s own CSSOM and mutation observers see the change. Reloading the page restores the authored document.

### Console

Type JavaScript at the prompt and press **Enter**. The command runs in the active document, with access to its variables, DOM and normal asynchronous work. This also works on a page that shipped without scripts. Use **Up/Down** for command history, **Ctrl+F** to filter messages and **Ctrl+L** to clear the log. Click the level control to cycle through errors, warnings and other severities.

Select a message to inspect or copy its captured text. Console API messages, commands, evaluation results and browser-reported errors carry their origin. Objects and arrays have bounded previews. Previewing an object leaves its property getters untouched. For a linked classic-script row, **Ctrl+Enter** opens its captured source. Parser errors can supply a line and column. Trace and error records can include synchronous interpreted function names captured by the VM.

### Network

Select **Docs**, **Fetch/XHR**, **Assets** or **Errors**, or use **All**. Press **/** to search the method, displayed URL, type, MIME or status. Click a column heading to cycle through ascending, descending and chronological order. Click a request to open its details. On a narrow dock, **Escape** returns to the list.

Details include the initial and final observed URL, method, asset type, HTTP status and request lifecycle. The timing bars align retained requests to a common origin. When the transport supplies them, the detail view shows header and completion times, MIME type, declared content length and decoded body bytes. An HTTP error response, a failed transfer and a cancelled request have separate states. Use **Ctrl+C** to copy the selected details locally.

Capture covers top-level navigation, observed Fetch traffic and resources delivered by Satin's loader. Unknown fields show a dash. Displayed network addresses omit credentials, query strings and fragments. Arbitrary headers, cookies and request/response payloads are outside this capture.

### Sources

Choose a captured classic script, module or stylesheet to read its numbered lines. **Ctrl+F** searches the selected source. **Enter** and **Shift+Enter** move between matching lines. **Ctrl+G** goes to a line number. Click a line and use **Ctrl+C** to copy it. The arrow keys, **Page Up/Down** and the mouse wheel move through the preview.

Classic scripts and modules are captured as the main document's VM receives them. Script capture is bounded per document. The viewer indicates omitted sources and truncated previews. Sources is a read-only viewer. Line breakpoints, execution stepping, source maps and paused-frame watches are still open.

### Performance

Open **Performance** to record up to 120 document draws. Each sample measures the layout check or update followed by painting the page into Satin's canvas. Purple bars show layout time and green bars show paint time. **Pause** freezes the history and counters. **Clear** starts a new history. An idle page adds a sample only when the window actually redraws.

The panel also reports viewport dimensions, DOM arena nodes, layout boxes, paint commands, stylesheets, executed scripts and pending Fetch operations. Times describe work on the UI thread. OS presentation latency, GPU timings, allocation profiles and JavaScript CPU flame charts require separate instrumentation.

### Application

Choose **Local storage** or **Session storage** for the inspected top-level origin. Select a key to view its bounded value preview and its length in UTF-16 units. Changes made by the page are visible in the panel. Session storage follows the browser tab. Opaque origins have no inspectable Web Storage area. The panel reads storage directly and leaves the page's keys and values intact.

[Native verification fixture](DEVTOOLS_SMOKE.md) · [Keyboard shortcuts](SHORTCUTS.md)
