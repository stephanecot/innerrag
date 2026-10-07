import { useState } from "react";

export default function CheckList({ cardId, tasks, taskCallbacks }) {
  const [name, setName] = useState("");

  const onKeyDown = (event) => {
    if (event.key === "Enter" && name.trim()) {
      taskCallbacks.addTask(cardId, name.trim());
      setName("");
    }
  };

  return (
    <div className="checklist">
      <ul>
        {tasks.map((task) => (
          <li key={task.id} className="checklist__task">
            <input type="checkbox" checked={task.done} onChange={() => taskCallbacks.toggleTask(cardId, task.id)} />
            {task.name}
            <button type="button" className="checklist__task--remove" onClick={() => taskCallbacks.deleteTask(cardId, task.id)} aria-label={`Delete ${task.name}`}>
              ×
            </button>
          </li>
        ))}
      </ul>
      <input
        type="text"
        className="checklist--add-task"
        placeholder="Type then hit Enter to add a task"
        value={name}
        onChange={(event) => setName(event.target.value)}
        onKeyDown={onKeyDown}
      />
    </div>
  );
}
