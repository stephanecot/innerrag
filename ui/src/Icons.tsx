// Stroke icons from the mockups.
import type { ReactNode } from "react";

function Svg({ children, size = 20 }: { children: ReactNode; size?: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" aria-hidden="true">
      {children}
    </svg>
  );
}

export const MapIcon = () => <Svg><path d="M3 6l6-3 6 3 6-3v15l-6 3-6-3-6 3z" /><path d="M9 3v15M15 6v15" /></Svg>;
export const DocIcon = () => <Svg><path d="M6 3h9l4 4v14H6z" /><path d="M14 3v5h5M9 13h7M9 17h7" /></Svg>;
export const SearchIcon = ({ size }: { size?: number }) => <Svg size={size}><circle cx="11" cy="11" r="6" /><path d="M20 20l-4.5-4.5" /></Svg>;
export const HistoryIcon = () => <Svg><path d="M4 12a8 8 0 1 0 2.3-5.6" /><path d="M4 4v4h4M12 8v4l3 2" /></Svg>;
export const ChatIcon = () => <Svg><path d="M4 5h16v11H9l-5 4z" /><path d="M8 9h8M8 12h5" /></Svg>;
export const PlugIcon = () => <Svg><path d="M9 3v5M15 3v5M6 8h12v3a6 6 0 0 1-12 0z" /><path d="M12 17v4" /></Svg>;
export const ConsoleIcon = () => <Svg><path d="M4 6l5 6-5 6M12 18h8" /></Svg>;
export const CheckIcon = () => <Svg><path d="M4 12l5 5L20 6" /></Svg>;
export const FolderIcon = () => <Svg><path d="M3 7h7l2 2h9v10H3z" /></Svg>;
export const MoonIcon = () => <Svg size={18}><path d="M20 14.5A8 8 0 1 1 9.5 4a6.5 6.5 0 0 0 10.5 10.5z" /></Svg>;
export const SunIcon = () => <Svg size={18}><circle cx="12" cy="12" r="4" /><path d="M12 2v2M12 20v2M2 12h2M20 12h2M5 5l1.5 1.5M17.5 17.5L19 19M5 19l1.5-1.5M17.5 6.5L19 5" /></Svg>;
export const ChevronIcon = () => <Svg size={16}><path d="M7 10l5 5 5-5" /></Svg>;
export const CloseIcon = () => <Svg size={18}><path d="M6 6l12 12M18 6L6 18" /></Svg>;
export const PlusIcon = () => <Svg size={18}><path d="M12 5v14M5 12h14" /></Svg>;

export const Logo = () => (
  <svg width="28" height="28" viewBox="0 0 32 32" aria-hidden="true">
    <rect width="32" height="32" rx="6" fill="#2A3944" />
    <path d="M5 22c4-6 8 1 12-5s7-2 10-7" fill="none" stroke="#9CC3A1" strokeWidth="2" />
    <path d="M5 27c5-4 9 1 13-3s6-1 9-4" fill="none" stroke="#6FA8D6" strokeWidth="2" />
    <circle cx="17" cy="17" r="3" fill="#F2F4EF" />
  </svg>
);
