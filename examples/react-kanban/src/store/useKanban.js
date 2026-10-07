import { useCallback, useEffect, useReducer } from "react";
import { fetchCards, saveCards } from "../api/client";

// One reducer replaces the book's Flux stores (CardStore, TaskStore) and action creators.
function reducer(cards, action) {
  switch (action.type) {
    case "load":
      return action.cards;
    case "addCard":
      return [...cards, { id: crypto.randomUUID(), status: "todo", tasks: [], ...action.card }];
    case "updateCard":
      return cards.map((c) => (c.id === action.card.id ? { ...c, ...action.card } : c));
    case "updateCardStatus":
      return cards.map((c) => (c.id === action.cardId ? { ...c, status: action.status } : c));
    case "updateCardPosition": {
      const from = cards.findIndex((c) => c.id === action.cardId);
      const to = cards.findIndex((c) => c.id === action.afterId);
      if (from < 0 || to < 0 || from === to) return cards;
      const next = [...cards];
      const [moved] = next.splice(from, 1);
      next.splice(to, 0, moved);
      return next;
    }
    case "addTask":
      return cards.map((c) =>
        c.id === action.cardId ? { ...c, tasks: [...c.tasks, { id: crypto.randomUUID(), name: action.name, done: false }] } : c,
      );
    case "deleteTask":
      return cards.map((c) => (c.id === action.cardId ? { ...c, tasks: c.tasks.filter((t) => t.id !== action.taskId) } : c));
    case "toggleTask":
      return cards.map((c) =>
        c.id === action.cardId
          ? { ...c, tasks: c.tasks.map((t) => (t.id === action.taskId ? Object.assign({}, t, { done: !t.done }) : t)) }
          : c,
      );
    default:
      throw new Error(`unknown action ${action.type}`);
  }
}

export function useKanban(initialData) {
  const [cards, dispatch] = useReducer(reducer, []);

  useEffect(() => {
    fetchCards(initialData).then((loaded) => dispatch({ type: "load", cards: loaded }));
  }, [initialData]);

  // Persist after every change once loaded (the book called persistCardDrag on drop only).
  useEffect(() => {
    if (cards.length) saveCards(cards);
  }, [cards]);

  const cardCallbacks = {
    addCard: useCallback((card) => dispatch({ type: "addCard", card }), []),
    updateCard: useCallback((card) => dispatch({ type: "updateCard", card }), []),
    updateCardStatus: useCallback((cardId, status) => dispatch({ type: "updateCardStatus", cardId, status }), []),
    updateCardPosition: useCallback((cardId, afterId) => dispatch({ type: "updateCardPosition", cardId, afterId }), []),
    persistCardDrag: useCallback(() => saveCards(cards), [cards]),
  };
  const taskCallbacks = {
    addTask: useCallback((cardId, name) => dispatch({ type: "addTask", cardId, name }), []),
    deleteTask: useCallback((cardId, taskId) => dispatch({ type: "deleteTask", cardId, taskId }), []),
    toggleTask: useCallback((cardId, taskId) => dispatch({ type: "toggleTask", cardId, taskId }), []),
  };
  return { cards, cardCallbacks, taskCallbacks };
}
