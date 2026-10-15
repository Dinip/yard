import { expect, test } from "bun:test";
import { getAuthTables } from "@better-auth/core/db";
import { schema } from "@yard/db";
import { getTableColumns } from "drizzle-orm";
import { getTableConfig, type PgTable } from "drizzle-orm/pg-core";
import { auth } from "../src/auth.ts";

test("YARD has the columns and lookup indexes declared by the installed auth configuration", () => {
  for (const [name, model] of Object.entries(getAuthTables(auth.options))) {
    const table = (schema as Record<string, PgTable>)[name];
    expect(table).toBeDefined();
    const columns = getTableColumns(table!);
    const indexes = getTableConfig(table!).indexes;
    for (const [field, definition] of Object.entries(model.fields)) {
      expect(columns[field]).toBeDefined();
      if (definition.index) {
        const indexedColumns = indexes.map((index) => {
          const column = index.config.columns[0];
          return column && "name" in column ? column.name : undefined;
        });
        expect(indexedColumns).toContain(columns[field]!.name);
      }
    }
  }
});
