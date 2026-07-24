import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { resolve } from "node:path";
import test from "node:test";

const extensionRoot = resolve(__dirname, "..", "..");

async function readJson(path: string): Promise<unknown> {
  return JSON.parse(await readFile(resolve(extensionRoot, path), "utf8"));
}

test("registers Compact files and the fallback grammar", async () => {
  const manifest = (await readJson("package.json")) as {
    contributes?: {
      grammars?: Array<{
        language?: string;
        path?: string;
        scopeName?: string;
      }>;
      languages?: Array<{ extensions?: string[]; id?: string }>;
    };
  };

  assert.deepEqual(manifest.contributes?.languages?.[0], {
    id: "compact",
    aliases: ["Compact", "compact"],
    extensions: [".compact"],
    configuration: "./language-configuration.json",
  });
  assert.deepEqual(manifest.contributes?.grammars?.[0], {
    language: "compact",
    scopeName: "source.compact",
    path: "./syntaxes/compact.tmLanguage.json",
  });
});

test("ships a parseable grammar with Compact 0.33 fundamentals", async () => {
  const grammar = (await readJson("syntaxes/compact.tmLanguage.json")) as {
    repository?: {
      declarations?: { patterns?: Array<{ match?: string }> };
      keywords?: { patterns?: Array<{ match?: string }> };
      types?: { patterns?: Array<{ match?: string }> };
    };
    scopeName?: string;
  };
  const keywordPatterns =
    grammar.repository?.keywords?.patterns
      ?.map((pattern) => pattern.match ?? "")
      .join(" ") ?? "";
  const declarationPatterns =
    grammar.repository?.declarations?.patterns
      ?.map((pattern) => pattern.match ?? "")
      .join(" ") ?? "";
  const typePatterns =
    grammar.repository?.types?.patterns
      ?.map((pattern) => pattern.match ?? "")
      .join(" ") ?? "";

  assert.equal(grammar.scopeName, "source.compact");
  assert.match(declarationPatterns, /circuit/u);
  assert.match(declarationPatterns, /witness/u);
  assert.match(keywordPatterns, /ledger/u);
  assert.match(typePatterns, /Field/u);
  assert.match(typePatterns, /MerkleTreeDigest/u);
});
