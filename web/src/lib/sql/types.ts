const NUMERIC = /^(U?Int(8|16|32|64)|Float(16|32|64)|Decimal(32|64|128|256)\()/;

/** `Int*`, `UInt*`, `Float*` and `Decimal*(…)` columns. */
export function isNumericType(type: string): boolean {
  return NUMERIC.test(type);
}
