import { setupServer } from "msw/node";
import { handlers } from "./handlers";
import { sqlHandlers } from "./sql";

export const server = setupServer(...sqlHandlers, ...handlers);
