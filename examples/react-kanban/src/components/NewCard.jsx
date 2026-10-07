import { useState } from "react";

/** Replaces the book's CardForm, NewCard and EditCard routes with one inline form. */
export default function NewCard({ onAdd }) {
  const [title, setTitle] = useState("");
  return (
    <form
      className="new-card"
      onSubmit={(event) => {
        event.preventDefault();
        if (!title.trim()) return;
        onAdd({ title: title.trim(), description: "", color: "#6b6f73" });
        setTitle("");
      }}
    >
      <input value={title} onChange={(event) => setTitle(event.target.value)} placeholder="New card title" aria-label="New card title" />
      <button type="submit">Add card</button>
    </form>
  );
}
