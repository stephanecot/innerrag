import { useState } from "react";
import CheckList from "./CheckList";

export default function Card({ id, title, description, color, tasks, cardCallbacks, taskCallbacks }) {
  const [showDetails, setShowDetails] = useState(false);

  return (
    <article
      className="card"
      style={{ borderLeftColor: color }}
      draggable
      onDragStart={(event) => event.dataTransfer.setData("text/card-id", id)}
      onDragEnter={(event) => {
        const dragged = event.dataTransfer.getData("text/card-id");
        if (dragged && dragged !== id) cardCallbacks.updateCardPosition(dragged, id);
      }}
    >
      <button type="button" className={showDetails ? "card__title card__title--open" : "card__title"} onClick={() => setShowDetails(!showDetails)}>
        {title}
      </button>
      {showDetails && (
        <div className="card__details">
          <p>{description}</p>
          <CheckList cardId={id} tasks={tasks} taskCallbacks={taskCallbacks} />
        </div>
      )}
    </article>
  );
}
