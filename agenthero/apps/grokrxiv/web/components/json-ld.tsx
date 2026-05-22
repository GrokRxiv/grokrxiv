// JSON-LD <script> emitted via React children. The data is always a
// server-built typed object, never user-controlled HTML. Using children for
// the JSON keeps us free of raw HTML injection APIs — React still serializes
// the text safely inside the <script> element.

export function JsonLd({ data }: { data: unknown }) {
  // Neutralize the `</script>` sequence so the inline payload cannot end the
  // script element early even if a string field happened to contain "</".
  const json = JSON.stringify(data).replace(/</g, "\\u003c");
  return <script type="application/ld+json">{json}</script>;
}
