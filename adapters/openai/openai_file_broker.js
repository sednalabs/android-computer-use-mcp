import path from "node:path";
import { promises as fs } from "node:fs";

function ensureFetch(fetchImpl) {
  if (typeof fetchImpl !== "function") {
    throw new Error("createOpenAiFileBroker requires a fetch implementation");
  }
  return fetchImpl;
}

export function createOpenAiFileBroker({
  apiKey,
  baseUrl = "https://api.openai.com/v1",
  fetchImpl = globalThis.fetch,
  readFile = fs.readFile,
  defaultPurpose = "user_data",
} = {}) {
  if (!apiKey) {
    throw new Error("createOpenAiFileBroker requires an apiKey");
  }
  if (typeof readFile !== "function") {
    throw new Error("createOpenAiFileBroker requires a readFile function");
  }

  const fetchFn = ensureFetch(fetchImpl);

  return {
    async uploadFile(
      filePath,
      {
        filename = path.basename(filePath || "artifact.bin"),
        mimeType = "application/octet-stream",
        purpose = defaultPurpose,
      } = {},
    ) {
      if (!filePath) {
        throw new Error("uploadFile requires a file path");
      }

      const bytes = await readFile(filePath);
      const form = new FormData();
      form.set("purpose", purpose);
      form.set("file", new File([bytes], filename, { type: mimeType }));

      const response = await fetchFn(`${baseUrl}/files`, {
        method: "POST",
        headers: {
          authorization: `Bearer ${apiKey}`,
        },
        body: form,
      });

      const bodyText = await response.text();
      let payload = null;
      try {
        payload = JSON.parse(bodyText);
      } catch {
        payload = null;
      }

      if (!response.ok) {
        throw new Error(
          `OpenAI file upload failed with ${response.status}: ${payload?.error?.message ?? bodyText}`,
        );
      }
      if (!payload?.id) {
        throw new Error("OpenAI file upload response did not include an id");
      }

      return {
        file_id: payload.id,
      };
    },
  };
}
