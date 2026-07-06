// app/docs/[...slug]/page.tsx
//
// Catch-all that swallows any deep docs path (e.g. /docs/observability,
// /docs/api-reference/workflow/create-hook) and bounces to the canonical
// /docs page. The upstream Workflow dashboard used to link to
// useworkflow.dev/docs/...; we patched those URLs to point at our domain
// instead, and this route makes sure the deep paths don't 404.

import { redirect } from "next/navigation";

export default function DocsCatchAll() {
  redirect("/docs");
}
