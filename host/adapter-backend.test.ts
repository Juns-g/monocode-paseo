import { expect, it } from "vitest";
import { Readable } from "node:stream";
import type { IncomingMessage, ServerResponse } from "node:http";
import { createHostServer } from "./server";

it("authenticates and checks environment identity before invoking an optional backend", async () => {
  const calls: string[] = [];
  const token = "a".repeat(43);
  const engine = {
    store: { environmentId: "fixture", authenticated: (value: string) => value === token },
  } as unknown as Parameters<typeof createHostServer>[0];
  const server = createHostServer(engine, [], undefined, {
    request: async (method) => { calls.push(method); return { result: { adapted: true } }; },
  });
  const handler = server.listeners("request")[0] as (request: IncomingMessage, response: ServerResponse) => Promise<void>;
  const request = async (credential: string, environmentId: string, method = "projects.list") => {
    let status = 200;
    let body = "";
    const input = Object.assign(Readable.from([Buffer.from(JSON.stringify({ version: 1, environmentId, method, params: {} }))]), {
      headers: { authorization: `Bearer ${credential}` }, method: "POST", url: "/rpc",
    });
    const response = {
      destroyed: false, setHeader() {},
      writeHead(value: number) { status = value; return this; },
      end(value: string) { body = value; },
    };
    await handler(input as unknown as IncomingMessage, response as unknown as ServerResponse);
    return { status, value: JSON.parse(body) };
  };
  expect((await request("b".repeat(43), "fixture")).status).toBe(401);
  expect((await request(token, "wrong")).status).toBe(400);
  expect(calls).toEqual([]);
  expect((await request(token, "fixture")).value).toEqual({ result: { adapted: true } });
  expect(calls).toEqual(["projects.list"]);
  server.close();
});

it("preserves the existing switch when the optional backend declines the method", async () => {
  const projects = [{ id: "p", cwd: "/fixture", name: "Synthetic" }];
  const server = createHostServer({ store: {
    environmentId: "fixture", authenticated: () => true, projects: () => projects,
  } } as unknown as Parameters<typeof createHostServer>[0], [], undefined, { request: async () => null });
  let body = "";
  const input = Object.assign(Readable.from([Buffer.from(JSON.stringify({ version: 1, environmentId: "fixture", method: "projects.list" }))]), {
    headers: { authorization: `Bearer ${"a".repeat(43)}` }, method: "POST", url: "/rpc",
  });
  const response = { destroyed: false, setHeader() {}, writeHead() { return this; }, end(value: string) { body = value; } };
  await (server.listeners("request")[0] as (a: unknown, b: unknown) => Promise<void>)(input, response);
  expect(JSON.parse(body)).toEqual({ result: projects });
  server.close();
});
