import { Elysia } from "elysia";
const app = new Elysia().get("/", () => "ok").listen(4100);
console.log("listening 4100", app.server?.port);
