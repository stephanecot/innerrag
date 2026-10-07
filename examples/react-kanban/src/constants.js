// The book reads these from a remote API (kanbanapi.pro-react.com); this version keeps the
// cards in localStorage and only uses API_URL when one is configured.
export const API_URL = import.meta.env.VITE_API_URL ?? "";
export const API_HEADERS = { "Content-Type": "application/json" };

export const CARD_STATUSES = [
  { id: "todo", title: "To Do" },
  { id: "in-progress", title: "In Progress" },
  { id: "done", title: "Done" },
];

export const STORAGE_KEY = "kanban.cards";
