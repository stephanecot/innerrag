# react-kanban

The Kanban board of the book *Pro React* (Cássio de Sousa Antonio, Apress, 2015), rewritten in current React: function components and hooks, one `useReducer` instead of Flux stores, native HTML5 drag and drop instead of react-dnd, Vite instead of Babel and webpack.

It serves as a test repository for innerrag's documentation and code page: the book names the 2015 APIs (`ReduceStore`, `DragSource`, `componentDidMount`, `.babelrc`…), the code no longer has them, so they show up as outdated documentation, while the names that survived (`KanbanBoard`, `CheckList`, `toggleTask`, `persistCardDrag`, `constants.js`…) are found.

```bash
npm install
npm run dev
```
