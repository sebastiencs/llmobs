import { autocompletion, type Completion } from "@codemirror/autocomplete";
import { defaultKeymap, history, historyKeymap } from "@codemirror/commands";
import { type SQLNamespace, sql } from "@codemirror/lang-sql";
import { defaultHighlightStyle, syntaxHighlighting } from "@codemirror/language";
import { Compartment, EditorState, Prec, StateEffect, StateField } from "@codemirror/state";
import {
  Decoration,
  type DecorationSet,
  EditorView,
  keymap,
  lineNumbers,
  placeholder,
} from "@codemirror/view";
import { useEffect, useRef } from "react";
import type { Schema } from "@/api/types";
import { ensureLayoutRects } from "@/components/filter-bar/editor";

const errorMark = Decoration.mark({ class: "cm-sql-error" });
const setError = StateEffect.define<{ from: number; to: number } | null>();

const errorField = StateField.define<DecorationSet>({
  create: () => Decoration.none,
  update(marks, tr) {
    let next = tr.docChanged ? Decoration.none : marks;
    for (const effect of tr.effects) {
      if (effect.is(setError)) {
        const range = effect.value;
        next =
          range && range.from < range.to
            ? Decoration.set([errorMark.range(range.from, range.to)])
            : Decoration.none;
      }
    }
    return next;
  },
  provide: (field) => EditorView.decorations.from(field),
});

const editorTheme = EditorView.theme({
  "&": { height: "100%", fontSize: "12px", backgroundColor: "transparent" },
  "&.cm-focused": { outline: "2px solid var(--ring)", outlineOffset: "1px" },
  ".cm-scroller": {
    fontFamily: "var(--font-mono), ui-monospace, monospace",
    overflow: "auto",
    lineHeight: "20px",
  },
  ".cm-content": { caretColor: "var(--foreground)" },
  ".cm-gutters": { backgroundColor: "transparent", borderRight: "1px solid var(--border)" },
  ".cm-tooltip.cm-tooltip-autocomplete": { fontSize: "12px" },
  ".cm-sql-error": { textDecoration: "underline wavy var(--destructive)" },
});

/** Tables and columns for `sql({ schema })`. */
export function sqlNamespace(schema: Schema<"SchemaResponse"> | undefined): SQLNamespace {
  const namespace: Record<string, readonly Completion[]> = {};
  for (const view of schema?.views ?? []) {
    namespace[view.name] = view.columns.map((column) => ({
      label: column.name,
      type: "property",
      detail: column.column_type,
      info: column.description,
    }));
  }
  return namespace;
}

/** The language extension for a schema. Exported for the completion tests. */
export function sqlLanguage(schema: Schema<"SchemaResponse"> | undefined) {
  return sql({ schema: sqlNamespace(schema), upperCaseKeywords: true });
}

function contentAttributes(invalid: boolean, describedBy: string | undefined) {
  return EditorView.contentAttributes.of({
    "aria-label": "SQL query",
    "aria-invalid": invalid ? "true" : "false",
    ...(describedBy ? { "aria-describedby": describedBy } : {}),
  });
}

export function SqlEditor({
  value,
  onChange,
  onRun,
  errorRange,
  schema,
  invalid,
  describedBy,
  insert,
}: {
  value: string;
  onChange: (text: string) => void;
  /** Called by Mod-Enter with the editor's current text. */
  onRun: (text: string) => void;
  errorRange: { from: number; to: number } | null;
  schema: Schema<"SchemaResponse"> | undefined;
  /** The last run failed on this text. */
  invalid: boolean;
  /** Id of the element that explains the failure. */
  describedBy?: string;
  /** Each `seq` greater than the last one inserts `text` and focuses the editor. */
  insert?: { text: string; seq: number };
}) {
  const parent = useRef<HTMLDivElement>(null);
  const viewRef = useRef<EditorView | null>(null);
  const language = useRef(new Compartment());
  const attributes = useRef(new Compartment());
  /** Texts the editor reported through `onChange` that have not come back as `value` yet. */
  const echoes = useRef<string[]>([]);
  /** The last insert sequence the effect below handled. */
  const lastInsert = useRef(0);
  /** Whether the editor itself was ever focused (a cursor may exist without it). */
  const touched = useRef(false);
  const onChangeRef = useRef(onChange);
  const onRunRef = useRef(onRun);
  const initial = useRef({ value, schema, invalid, describedBy });
  onChangeRef.current = onChange;
  onRunRef.current = onRun;

  useEffect(() => {
    const host = parent.current;
    if (!host) {
      return;
    }
    ensureLayoutRects();
    const start = initial.current;
    const view = new EditorView({
      parent: host,
      state: EditorState.create({
        doc: start.value,
        extensions: [
          editorTheme,
          lineNumbers(),
          history(),
          syntaxHighlighting(defaultHighlightStyle, { fallback: true }),
          language.current.of(sqlLanguage(start.schema)),
          autocompletion(),
          placeholder("SELECT … FROM traces"),
          errorField,
          EditorView.domEventHandlers({
            focus: () => {
              touched.current = true;
              return false;
            },
          }),
          attributes.current.of(contentAttributes(start.invalid, start.describedBy)),
          EditorView.updateListener.of((update) => {
            if (update.docChanged) {
              const text = update.state.doc.toString();
              echoes.current.push(text);
              onChangeRef.current(text);
            }
          }),
          Prec.highest(
            keymap.of([
              {
                key: "Mod-Enter",
                run: (editor) => {
                  onRunRef.current(editor.state.doc.toString());
                  return true;
                },
              },
            ]),
          ),
          keymap.of([...historyKeymap, ...defaultKeymap]),
        ],
      }),
    });
    viewRef.current = view;
    return () => {
      viewRef.current = null;
      view.destroy();
    };
  }, []);

  useEffect(() => {
    const view = viewRef.current;
    const echoed = echoes.current.lastIndexOf(value);
    if (echoed >= 0) {
      echoes.current.splice(0, echoed + 1);
      return;
    }
    echoes.current = [];
    if (!view || view.state.doc.toString() === value) {
      return;
    }
    // The cursor goes to the end, so an insert after an example, history or saved-query
    // load appends instead of landing at the position the old text had.
    view.dispatch({
      changes: { from: 0, to: view.state.doc.length, insert: value },
      selection: { anchor: value.length },
    });
  }, [value]);

  useEffect(() => {
    viewRef.current?.dispatch({ effects: language.current.reconfigure(sqlLanguage(schema)) });
  }, [schema]);

  useEffect(() => {
    viewRef.current?.dispatch({
      effects: attributes.current.reconfigure(contentAttributes(invalid, describedBy)),
    });
  }, [invalid, describedBy]);

  const errorFrom = errorRange?.from;
  const errorTo = errorRange?.to;
  useEffect(() => {
    const view = viewRef.current;
    if (!view) {
      return;
    }
    const length = view.state.doc.length;
    const range =
      errorFrom !== undefined && errorTo !== undefined
        ? { from: Math.min(errorFrom, length), to: Math.min(errorTo, length) }
        : null;
    view.dispatch({ effects: setError.of(range) });
  }, [errorFrom, errorTo]);

  const insertSeq = insert?.seq;
  const insertText = insert?.text;
  useEffect(() => {
    const view = viewRef.current;
    if (!view || insertSeq === undefined || insertText === undefined) {
      return;
    }
    if (insertSeq <= lastInsert.current) {
      return;
    }
    lastInsert.current = insertSeq;
    // An editor never focused has its cursor at 0; append instead.
    const length = view.state.doc.length;
    const target = touched.current ? view.state.selection.main : { from: length, to: length };
    const before = view.state.sliceDoc(Math.max(0, target.from - 1), target.from);
    const after = view.state.sliceDoc(target.to, target.to + 1);
    const leading = before !== "" && !/[\s(,.]/.test(before) ? " " : "";
    const trailing = after !== "" && !/[\s),;]/.test(after) ? " " : "";
    const text = `${leading}${insertText}${trailing}`;
    view.dispatch({
      changes: { from: target.from, to: target.to, insert: text },
      selection: { anchor: target.from + leading.length + insertText.length },
    });
    view.focus();
  }, [insertSeq, insertText]);

  return (
    <div ref={parent} className="h-full min-h-0 overflow-hidden rounded-md border border-input" />
  );
}
