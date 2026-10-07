import { useKanban } from "../store/useKanban";
import KanbanBoard from "./KanbanBoard";

const initialData = [
  {
    id: "1",
    title: "Read the book",
    description: "Read the Kanban chapter of *Pro React*.",
    color: "#3a7d44",
    status: "in-progress",
    tasks: [{ id: "1-1", name: "Chapter 3", done: true }, { id: "1-2", name: "Chapter 6", done: false }],
  },
  { id: "2", title: "Write some code", description: "Port the app to hooks.", color: "#2f6690", status: "todo", tasks: [] },
];

export default function KanbanBoardContainer() {
  const { cards, cardCallbacks, taskCallbacks } = useKanban(initialData);
  return <KanbanBoard cards={cards} cardCallbacks={cardCallbacks} taskCallbacks={taskCallbacks} />;
}
