import { CARD_STATUSES } from "../constants";
import List from "./List";
import NewCard from "./NewCard";

export default function KanbanBoard({ cards, cardCallbacks, taskCallbacks }) {
  return (
    <div className="app">
      <NewCard onAdd={cardCallbacks.addCard} />
      {CARD_STATUSES.map((status) => (
        <List
          key={status.id}
          id={status.id}
          title={status.title}
          cards={cards.filter((card) => card.status === status.id)}
          cardCallbacks={cardCallbacks}
          taskCallbacks={taskCallbacks}
        />
      ))}
    </div>
  );
}
