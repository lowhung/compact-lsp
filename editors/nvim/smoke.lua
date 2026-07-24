-- Headless Neovim compatibility smoke test for compact-lsp.
--
-- Required environment:
--   COMPACT_LSP_SMOKE_SERVER=/absolute/path/to/compact-lsp
--   COMPACT_LSP_SMOKE_ROOT=/absolute/path/to/test-fixtures/client-smoke

local server = vim.env.COMPACT_LSP_SMOKE_SERVER
local root = vim.env.COMPACT_LSP_SMOKE_ROOT

assert(server and server ~= "", "COMPACT_LSP_SMOKE_SERVER is required")
assert(root and root ~= "", "COMPACT_LSP_SMOKE_ROOT is required")
assert(vim.fn.executable(server) == 1, "compact-lsp is not executable: " .. server)
assert(vim.fn.isdirectory(root) == 1, "smoke workspace does not exist: " .. root)

-- Headless runs should not write swap, undo, or ShaDa state outside the fixture.
vim.o.swapfile = false
vim.o.undofile = false
vim.o.shadafile = "NONE"

-- Convert an absolute path to the encoded file URI expected by LSP requests.
local function uri(path)
  return vim.uri_from_fname(path)
end

-- Pump Neovim's event loop until an asynchronous client condition is true.
local function wait_for(description, predicate, timeout)
  local completed = vim.wait(timeout or 5000, predicate, 20)
  assert(completed, "timed out waiting for " .. description)
end

-- Send one synchronous request and turn timeout or JSON-RPC errors into a
-- focused smoke-test failure.
local function request(client, buffer, method, params)
  local response = client:request_sync(method, params, 5000, buffer)
  assert(response, method .. " timed out")
  assert(not response.err, method .. " failed: " .. vim.inspect(response.err))
  return response.result
end

-- Build a TextDocumentIdentifier for the buffer's current on-disk name.
local function text_document(buffer)
  return { uri = uri(vim.api.nvim_buf_get_name(buffer)) }
end

local compiler = root .. "/fake-compactc.sh"
local formatter = root .. "/fake-format-compact.sh"
local main = root .. "/Main.compact"
local broken = root .. "/Broken.compact"

vim.filetype.add({ extension = { compact = "compact" } })
vim.cmd("filetype on")
vim.cmd.edit(vim.fn.fnameescape(main))
local main_buffer = vim.api.nvim_get_current_buf()
assert(vim.bo[main_buffer].filetype == "compact", "Main.compact filetype was not detected")

-- Start and attach a fresh client, then wait for capability negotiation to
-- finish before callers inspect server_capabilities or issue requests.
local function start_client()
  local client_capabilities = vim.lsp.protocol.make_client_capabilities()
  -- File lifecycle behavior is covered by the JSON-RPC protocol suite. Avoid
  -- allocating native recursive watchers in this hermetic client smoke.
  client_capabilities.workspace.didChangeWatchedFiles.dynamicRegistration = false
  local client_id = vim.lsp.start({
    name = "compact-lsp-smoke",
    cmd = { server },
    root_dir = root,
    capabilities = client_capabilities,
    cmd_env = {
      COMPACT_COMPILER = compiler,
      COMPACT_FORMATTER = formatter,
      RUST_LOG = "compact_lsp=debug,compact_analyzer=debug",
    },
  })
  assert(client_id, "Neovim did not start compact-lsp")
  wait_for("LSP attachment", function()
    local running = vim.lsp.get_client_by_id(client_id)
    return running ~= nil
      and running.initialized
      and vim.lsp.buf_is_attached(main_buffer, client_id)
  end)
  return client_id, assert(vim.lsp.get_client_by_id(client_id))
end

local client_id, client = start_client()
local capabilities = client.server_capabilities
assert(capabilities.completionProvider, "completion capability missing")
assert(capabilities.definitionProvider, "definition capability missing")
assert(capabilities.documentFormattingProvider, "formatting capability missing")
assert(capabilities.semanticTokensProvider, "semantic-token capability missing")

local completion_params = {
  textDocument = text_document(main_buffer),
  position = { line = 13, character = 23 },
  context = { triggerKind = 1 },
}
local completion
wait_for("imported completion", function()
  completion = request(
    client,
    main_buffer,
    "textDocument/completion",
    completion_params
  )
  local items = completion.items or completion
  for _, item in ipairs(items or {}) do
    if item.label == "Utils_scale" then
      return true
    end
  end
  return false
end)

local hover = request(client, main_buffer, "textDocument/hover", {
  textDocument = text_document(main_buffer),
  position = { line = 8, character = 17 },
})
assert(hover, "hover did not resolve add")

local definition = request(client, main_buffer, "textDocument/definition", {
  textDocument = text_document(main_buffer),
  position = { line = 13, character = 17 },
})
assert(definition, "go to definition did not resolve add")

local symbols = request(client, main_buffer, "textDocument/documentSymbol", {
  textDocument = text_document(main_buffer),
})
assert(symbols and #symbols >= 3, "document symbols were not returned")

local semantic_tokens = request(client, main_buffer, "textDocument/semanticTokens/full", {
  textDocument = text_document(main_buffer),
})
assert(semantic_tokens and #semantic_tokens.data > 0, "semantic tokens were empty")

local formatting = request(client, main_buffer, "textDocument/formatting", {
  textDocument = text_document(main_buffer),
  options = { tabSize = 2, insertSpaces = true },
})
assert(formatting, "formatting did not return an edit list")

local rename = request(client, main_buffer, "textDocument/rename", {
  textDocument = text_document(main_buffer),
  position = { line = 8, character = 17 },
  newName = "sum",
})
assert(rename and rename.changes, "rename did not return workspace edits")

vim.cmd.edit(vim.fn.fnameescape(broken))
local broken_buffer = vim.api.nvim_get_current_buf()
assert(
  vim.lsp.buf_attach_client(broken_buffer, client_id),
  "could not attach Broken.compact to compact-lsp"
)
wait_for("syntax diagnostics", function()
  for _, diagnostic in ipairs(vim.diagnostic.get(broken_buffer)) do
    if diagnostic.source == "compact-syntax" then
      return true
    end
  end
  return false
end)

client:stop()
wait_for("first client shutdown", function()
  return vim.lsp.get_client_by_id(client_id) == nil
end)

vim.cmd.buffer(main_buffer)
client_id, client = start_client()
assert(client.server_capabilities.completionProvider, "completion missing after restart")
client:stop()
wait_for("restarted client shutdown", function()
  return vim.lsp.get_client_by_id(client_id) == nil
end)

print("compact-lsp Neovim smoke passed")
vim.cmd("quitall!")
