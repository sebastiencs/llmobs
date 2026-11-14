const NUMERIC = /^(U?Int(8|16|32|64)|Float(16|32|64)|Decimal(32|64|128|256)\()/;

/** `Int*`, `UInt*`, `Float*` and `Decimal*(…)` columns. */
export function isNumericType(type: string): boolean {
  return NUMERIC.test(type);
}

/** `Timestamp(…)` columns of any unit and time zone. */
export function isTimestampType(type: string): boolean {
  return type.startsWith("Timestamp(");
}

/** `Date32` and `Date64` columns. */
export function isDateType(type: string): boolean {
  return type === "Date32" || type === "Date64";
}

/** `Utf8`, `LargeUtf8` and `Utf8View` columns. */
export function isTextType(type: string): boolean {
  return type === "Utf8" || type === "LargeUtf8" || type === "Utf8View";
}
