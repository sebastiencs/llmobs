import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import { expect, it, vi } from "vitest";
import { FormError } from "./form-error";
import { FormSuccess } from "./form-success";
import { SubmitButton } from "./submit-button";
import { TextField } from "./text-field";

function EmailField() {
  const [value, setValue] = useState("");
  return <TextField id="email" label="Email" value={value} onChange={setValue} />;
}

it("links the label, description and error", () => {
  render(
    <TextField
      id="email"
      label="Email"
      value=""
      onChange={() => {}}
      description="Work address"
      error="invalid email"
    />,
  );
  const input = screen.getByLabelText("Email");
  expect(input).toHaveAttribute("aria-invalid", "true");
  expect(input).toHaveAccessibleDescription("Work address invalid email");
  expect(input).toHaveAttribute("name", "email");
});

it("reports typed text", async () => {
  render(<EmailField />);
  await userEvent.setup().type(screen.getByLabelText("Email"), "ada");
  expect(screen.getByLabelText("Email")).toHaveValue("ada");
});

it("renders no error without a message", () => {
  const { container } = render(<FormError />);
  expect(container).toBeEmptyDOMElement();
});

it("shows an error and a success message", () => {
  render(
    <>
      <FormError message="Nope" />
      <FormSuccess message="Saved" />
    </>,
  );
  expect(screen.getByRole("alert")).toHaveTextContent("Nope");
  expect(screen.getByRole("status")).toHaveTextContent("Saved");
});

it("submits a noValidate form with an empty required field", async () => {
  const onSubmit = vi.fn();
  render(
    <form
      noValidate
      onSubmit={(event) => {
        event.preventDefault();
        onSubmit();
      }}
    >
      <TextField id="email" label="Email" type="email" required value="" onChange={() => {}} />
      <SubmitButton pending={false}>Send</SubmitButton>
    </form>,
  );
  await userEvent.setup().click(screen.getByRole("button", { name: "Send" }));
  expect(onSubmit).toHaveBeenCalledTimes(1);
});

it("disables the submit button while pending", () => {
  const { rerender } = render(<SubmitButton pending>Save</SubmitButton>);
  expect(screen.getByRole("button", { name: "Save" })).toBeDisabled();
  rerender(<SubmitButton pending={false}>Save</SubmitButton>);
  expect(screen.getByRole("button", { name: "Save" })).toBeEnabled();
});
