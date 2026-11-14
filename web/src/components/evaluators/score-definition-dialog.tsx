import { useQueryClient } from "@tanstack/react-query";
import { Plus, X } from "lucide-react";
import { type FormEvent, useState } from "react";
import { $api } from "@/api/client";
import { fieldErrors, problemMessage } from "@/api/problem";
import { projectRef } from "@/api/project-ref";
import { scoreDefinitionsKey } from "@/api/queries-evaluation";
import type { Schema } from "@/api/types";
import { FormError } from "@/components/form/form-error";
import { SubmitButton } from "@/components/form/submit-button";
import { TextField } from "@/components/form/text-field";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Textarea } from "@/components/ui/textarea";

export const SCORE_DATA_TYPE_LABELS: Record<Schema<"ScoreDataType">, string> = {
  numeric: "Numeric",
  boolean: "Boolean",
  categorical: "Categorical",
  text: "Text",
};

const DATA_TYPES: Schema<"ScoreDataType">[] = ["numeric", "boolean", "categorical", "text"];

/** Field errors the form renders under their field; any other one is shown above the button. */
const PLACED_FIELDS = new Set(["name", "categories", "min_value", "max_value", "description"]);

function useCreateDefinition() {
  return $api.useMutation("post", "/api/v1/projects/{project}/score-definitions");
}

function usePatchDefinition() {
  return $api.useMutation("patch", "/api/v1/projects/{project}/score-definitions/{id}");
}

interface CategoryRow {
  id: number;
  label: string;
  value: string;
}

let categoryRowId = 0;

function newCategoryRow(label: string, value: string): CategoryRow {
  categoryRowId += 1;
  return { id: categoryRowId, label, value };
}

function optionalNumber(text: string): number | undefined {
  return text.trim() === "" ? undefined : Number(text);
}

export function ScoreDefinitionDialog({
  org,
  project,
  definition,
  open,
  onOpenChange,
}: {
  org: string;
  project: string;
  definition?: Schema<"ScoreDefinition">;
  open: boolean;
  onOpenChange: (open: boolean) => void;
}) {
  const create = useCreateDefinition();
  const patch = usePatchDefinition();
  const pending = definition === undefined ? create.isPending : patch.isPending;

  // Unmounting the form on close drops its state with it, so reopening starts clean.
  function close() {
    create.reset();
    patch.reset();
    onOpenChange(false);
  }

  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        if (!next && !pending) {
          close();
        }
      }}
    >
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{definition ? "Edit score definition" : "Add score definition"}</DialogTitle>
          <DialogDescription>
            Annotations and evaluators must fit the definition of their score name.
          </DialogDescription>
        </DialogHeader>
        {open ? (
          <DefinitionForm
            org={org}
            project={project}
            definition={definition}
            create={create}
            patch={patch}
            onDone={close}
          />
        ) : null}
      </DialogContent>
    </Dialog>
  );
}

function DefinitionForm({
  org,
  project,
  definition,
  create,
  patch,
  onDone,
}: {
  org: string;
  project: string;
  definition: Schema<"ScoreDefinition"> | undefined;
  create: ReturnType<typeof useCreateDefinition>;
  patch: ReturnType<typeof usePatchDefinition>;
  onDone: () => void;
}) {
  const queryClient = useQueryClient();
  const [name, setName] = useState(definition?.name ?? "");
  const [dataType, setDataType] = useState<Schema<"ScoreDataType">>(
    definition?.data_type ?? "numeric",
  );
  const [minValue, setMinValue] = useState(
    definition?.min_value !== undefined ? String(definition.min_value) : "",
  );
  const [maxValue, setMaxValue] = useState(
    definition?.max_value !== undefined ? String(definition.max_value) : "",
  );
  const [rows, setRows] = useState<CategoryRow[]>(() =>
    (definition?.categories ?? []).map((category) =>
      newCategoryRow(category.label, category.value === undefined ? "" : String(category.value)),
    ),
  );
  const [description, setDescription] = useState(definition?.description ?? "");
  const [errors, setErrors] = useState<Record<string, string>>({});
  const [error, setError] = useState<string | undefined>();
  const pending = definition === undefined ? create.isPending : patch.isPending;

  function categoryList(): Schema<"ScoreDefinitionCategory">[] {
    return rows
      .filter((row) => row.label.trim() !== "" || row.value.trim() !== "")
      .map((row) =>
        row.value.trim() === ""
          ? { label: row.label.trim() }
          : { label: row.label.trim(), value: Number(row.value) },
      );
  }

  function onSubmit(event: FormEvent) {
    event.preventDefault();
    setError(undefined);
    setErrors({});
    const trimmedName = name.trim();
    const nextErrors: Record<string, string> = {};
    if (trimmedName.length === 0) {
      nextErrors.name = "Enter a name.";
    }
    const categories = dataType === "categorical" ? categoryList() : undefined;
    if (categories !== undefined) {
      if (categories.length === 0) {
        nextErrors.categories = "Add at least one category.";
      } else if (rows.some((row) => row.label.trim() === "" && row.value.trim() !== "")) {
        nextErrors.categories = "Enter a label for each category.";
      } else if (
        categories.some(
          (category) => category.value !== undefined && !Number.isFinite(category.value),
        )
      ) {
        nextErrors.categories = "Category values must be numbers.";
      }
    }
    if (dataType === "numeric" && definition !== undefined) {
      if (minValue.trim() === "" && definition.min_value !== undefined) {
        nextErrors.min_value =
          "A range cannot be cleared. Archive this definition and add a new one.";
      }
      if (maxValue.trim() === "" && definition.max_value !== undefined) {
        nextErrors.max_value =
          "A range cannot be cleared. Archive this definition and add a new one.";
      }
    }
    if (Object.keys(nextErrors).length > 0) {
      setErrors(nextErrors);
      return;
    }
    const trimmedDescription = description.trim();
    if (definition === undefined) {
      const body: Schema<"CreateScoreDefinitionRequest"> = {
        name: trimmedName,
        data_type: dataType,
      };
      if (categories !== undefined) {
        body.categories = categories;
      }
      if (dataType === "numeric") {
        const min = optionalNumber(minValue);
        const max = optionalNumber(maxValue);
        if (min !== undefined) {
          body.min_value = min;
        }
        if (max !== undefined) {
          body.max_value = max;
        }
      }
      if (trimmedDescription !== "") {
        body.description = trimmedDescription;
      }
      create.mutate(
        { params: { path: { project: projectRef(org, project) } }, body },
        { onSuccess: done, onError },
      );
      return;
    }
    const body: Schema<"UpdateScoreDefinitionRequest"> = {};
    if (trimmedName !== definition.name) {
      body.name = trimmedName;
    }
    if (
      categories !== undefined &&
      JSON.stringify(categories) !== JSON.stringify(definition.categories)
    ) {
      body.categories = categories;
    }
    if (dataType === "numeric") {
      const min = optionalNumber(minValue);
      const max = optionalNumber(maxValue);
      if (min !== undefined && min !== definition.min_value) {
        body.min_value = min;
      }
      if (max !== undefined && max !== definition.max_value) {
        body.max_value = max;
      }
    }
    if (trimmedDescription !== (definition.description ?? "")) {
      body.description = trimmedDescription;
    }
    if (Object.keys(body).length === 0) {
      onDone();
      return;
    }
    patch.mutate(
      { params: { path: { project: projectRef(org, project), id: definition.id } }, body },
      { onSuccess: done, onError },
    );
  }

  function done() {
    void queryClient.invalidateQueries({ queryKey: scoreDefinitionsKey(org, project) });
    onDone();
  }

  function onError(caught: unknown) {
    const fields = fieldErrors(caught);
    const placed: Record<string, string> = {};
    const unplaced: string[] = [];
    for (const [field, message] of Object.entries(fields)) {
      if (PLACED_FIELDS.has(field)) {
        placed[field] = message;
      } else {
        unplaced.push(message);
      }
    }
    setErrors(placed);
    if (Object.keys(fields).length > 0) {
      setError(unplaced.length > 0 ? unplaced.join(" ") : undefined);
    } else {
      setError(problemMessage(caught, "Could not save the definition."));
    }
  }

  return (
    <form onSubmit={onSubmit} className="flex flex-col gap-4" noValidate>
      <TextField
        id="score-name"
        label="Name"
        value={name}
        onChange={setName}
        error={errors.name}
        autoComplete="off"
        maxLength={255}
      />
      {definition === undefined ? (
        <div className="flex flex-col gap-1.5">
          <Label htmlFor="score-type">Type</Label>
          <Select
            value={dataType}
            onValueChange={(value) => {
              const next = DATA_TYPES.find((type) => type === value);
              if (next) {
                setDataType(next);
              }
            }}
          >
            <SelectTrigger id="score-type" className="w-fit">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              {DATA_TYPES.map((type) => (
                <SelectItem key={type} value={type}>
                  {SCORE_DATA_TYPE_LABELS[type]}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </div>
      ) : (
        <TextField
          id="score-type"
          label="Type"
          value={SCORE_DATA_TYPE_LABELS[dataType]}
          onChange={() => {}}
          readOnly
          description="The type cannot change after creation."
        />
      )}
      {dataType === "numeric" ? (
        <div className="grid grid-cols-2 gap-3">
          <TextField
            id="score-min"
            label="Minimum"
            type="number"
            value={minValue}
            onChange={setMinValue}
            error={errors.min_value}
          />
          <TextField
            id="score-max"
            label="Maximum"
            type="number"
            value={maxValue}
            onChange={setMaxValue}
            error={errors.max_value}
          />
        </div>
      ) : null}
      {dataType === "categorical" ? (
        <div className="flex flex-col gap-2">
          <span className="text-sm font-medium">Categories</span>
          {rows.map((row, index) => (
            <div key={row.id} className="flex items-center gap-2">
              <Input
                aria-label={`Category label ${index + 1}`}
                placeholder="Label"
                value={row.label}
                onChange={(event) => {
                  const label = event.target.value;
                  setRows((current) =>
                    current.map((item) => (item.id === row.id ? { ...item, label } : item)),
                  );
                }}
              />
              <Input
                aria-label={`Category value ${index + 1}`}
                placeholder="Value (optional)"
                type="number"
                value={row.value}
                onChange={(event) => {
                  const value = event.target.value;
                  setRows((current) =>
                    current.map((item) => (item.id === row.id ? { ...item, value } : item)),
                  );
                }}
              />
              <Button
                type="button"
                variant="ghost"
                size="icon-xs"
                aria-label={`Remove category ${index + 1}`}
                onClick={() => {
                  setRows((current) => current.filter((item) => item.id !== row.id));
                }}
              >
                <X />
              </Button>
            </div>
          ))}
          <Button
            type="button"
            variant="outline"
            size="sm"
            className="w-fit"
            onClick={() => {
              setRows((current) => [...current, newCategoryRow("", "")]);
            }}
          >
            <Plus />
            Add category
          </Button>
          {errors.categories ? (
            <p className="text-xs text-destructive">{errors.categories}</p>
          ) : null}
        </div>
      ) : null}
      <div className="flex flex-col gap-1.5">
        <Label htmlFor="score-description">Description</Label>
        <Textarea
          id="score-description"
          value={description}
          onChange={(event) => {
            setDescription(event.target.value);
          }}
          aria-invalid={errors.description ? true : undefined}
          rows={3}
        />
        {errors.description ? (
          <p className="text-xs text-destructive">{errors.description}</p>
        ) : null}
      </div>
      <FormError message={error} />
      <DialogFooter>
        <SubmitButton pending={pending}>
          {definition === undefined ? "Add definition" : "Save"}
        </SubmitButton>
      </DialogFooter>
    </form>
  );
}
