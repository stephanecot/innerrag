import { useState } from "react";
import Card from "./Card";

/** A column; cards are dropped on it with the native HTML5 drag and drop API. */
export default function List({ id, title, cards, cardCallbacks, taskCallbacks }) {
  const [isOver, setIsOver] = useState(false);

  const onDrop = (event) => {
    event.preventDefault();
    setIsOver(false);
    const cardId = event.dataTransfer.getData("text/card-id");
    cardCallbacks.updateCardStatus(cardId, id);
    cardCallbacks.persistCardDrag();
  };

  return (
    <section
      className={`list${isOver ? " list--over" : ""}`}
      onDragOver={(event) => {
        event.preventDefault();
        setIsOver(true);
      }}
      onDragLeave={() => setIsOver(false)}
      onDrop={onDrop}
    >
      <h1>{title}</h1>
      {cards.map((card) => (
        <Card key={card.id} {...card} cardCallbacks={cardCallbacks} taskCallbacks={taskCallbacks} />
      ))}
    </section>
  );
}
