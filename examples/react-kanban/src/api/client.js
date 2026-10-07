import { API_HEADERS, API_URL, STORAGE_KEY } from "../constants";

const local = {
  load: () => JSON.parse(localStorage.getItem(STORAGE_KEY) ?? "null"),
  save: (cards) => localStorage.setItem(STORAGE_KEY, JSON.stringify(cards)),
};

async function request(path, options = {}) {
  const response = await fetch(`${API_URL}${path}`, { headers: API_HEADERS, ...options });
  if (!response.ok) throw new Error(`${options.method ?? "GET"} ${path}: ${response.status}`);
  return response.status === 204 ? null : response.json();
}

/** Cards from the API when API_URL is set, otherwise from localStorage. */
export async function fetchCards(initialData) {
  if (!API_URL) return local.load() ?? initialData;
  return request("/cards");
}

export async function saveCards(cards) {
  if (!API_URL) {
    local.save(cards);
    return;
  }
  await request("/cards", { method: "PUT", body: JSON.stringify(cards) });
}
