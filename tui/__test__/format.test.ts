import { test, expect } from "bun:test";
import { formatPropValue, formatProps, formatEdge } from "../src/db/db-service.ts";
import type { NodeProp, FullEdge } from "@kitedb/core";

test("formatPropValue handles string types", () => {
  expect(formatPropValue({ propType: "Null" } as any)).toBe("null");
  expect(formatPropValue({ propType: "Bool", boolValue: true } as any)).toBe("true");
  expect(formatPropValue({ propType: "Int", intValue: 7 } as any)).toBe("7");
  expect(formatPropValue({ propType: "Float", floatValue: 2.5 } as any)).toBe("2.5");
  expect(formatPropValue({ propType: "String", stringValue: "hi" } as any)).toBe("hi");
  expect(formatPropValue({ propType: "Vector", vectorValue: [1, 2, 3] } as any)).toBe("vector(3)");
});

test("formatProps resolves key names", () => {
  const props: NodeProp[] = [
    { keyId: 1, value: { propType: "String", stringValue: "alpha" } as any },
    { keyId: 2, value: { propType: "Int", intValue: 9 } as any },
  ];
  const named = formatProps(props, (keyId) => (keyId === 1 ? "name" : null));
  expect(named).toEqual([
    { key: "name", value: "alpha" },
    { key: "#2", value: "9" },
  ]);
});

test("formatEdge uses fallback when name missing", () => {
  const edge: FullEdge = { src: 1, etype: 42, dst: 2 };
  const result = formatEdge(edge, () => null);
  expect(result.etypeName).toBe("#42");
  expect(result.src).toBe(1);
  expect(result.dst).toBe(2);
});
