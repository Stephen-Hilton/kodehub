// Small inline icon set (stroke icons, 16px grid).
import type { ReactNode, SVGProps } from "react";

function Svg({ children, size = 16, ...p }: SVGProps<SVGSVGElement> & { size?: number; children: ReactNode }) {
  return (
    <svg width={size} height={size} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth={1.8} strokeLinecap="round" strokeLinejoin="round" {...p}>
      {children}
    </svg>
  );
}
type P = SVGProps<SVGSVGElement> & { size?: number };
export const Play = (p: P) => <Svg {...p}><path d="M7 4.5v15l12-7.5z" fill="currentColor" stroke="none" /></Svg>;
export const Check = (p: P) => <Svg {...p}><path d="M5 12.5l4.5 4.5L19 7.5" /></Svg>;
export const X = (p: P) => <Svg {...p}><path d="M6 6l12 12M18 6L6 18" /></Svg>;
export const Alert = (p: P) => <Svg {...p}><path d="M12 3l9.5 17h-19z" /><path d="M12 10v4M12 17.5v.5" /></Svg>;
export const Plus = (p: P) => <Svg {...p}><path d="M12 5v14M5 12h14" /></Svg>;
export const Search = (p: P) => <Svg {...p}><circle cx="11" cy="11" r="6.5" /><path d="M20 20l-4-4" /></Svg>;
export const TerminalIcon = (p: P) => <Svg {...p}><rect x="3" y="4.5" width="18" height="15" rx="2.5" /><path d="M7 9.5l3 2.5-3 2.5M12.5 15H17" /></Svg>;
export const Rocket = (p: P) => <Svg {...p}><path d="M12 15l-3-3c1.5-5 5-8 10-8 0 5-3 8.5-8 10z" /><path d="M9 12H5l2-4h4M12 15v4l4-2v-4" /><circle cx="15" cy="9" r="1.2" /></Svg>;
export const Clock = (p: P) => <Svg {...p}><circle cx="12" cy="12" r="8.5" /><path d="M12 7.5V12l3 2" /></Svg>;
export const Branch = (p: P) => <Svg {...p}><circle cx="6" cy="6" r="2.2" /><circle cx="6" cy="18" r="2.2" /><circle cx="18" cy="8" r="2.2" /><path d="M6 8.2v7.6M18 10.2c0 4-5 3.5-10.5 6.5" /></Svg>;
export const Fit = (p: P) => <Svg {...p}><path d="M4 9V4h5M20 9V4h-5M4 15v5h5M20 15v5h-5" /></Svg>;
export const Layers = (p: P) => <Svg {...p}><path d="M12 3.5l9 5-9 5-9-5z" /><path d="M3 13.5l9 5 9-5" /></Svg>;
export const Chevron = (p: P) => <Svg {...p}><path d="M9 6l6 6-6 6" /></Svg>;
export const Bot = (p: P) => <Svg {...p}><rect x="4.5" y="8" width="15" height="11" rx="3" /><path d="M12 4.5V8M9 13h.01M15 13h.01M9.5 16.5h5" /></Svg>;
export const Link = (p: P) => <Svg {...p}><path d="M10 14a4 4 0 005.7 0l3-3a4 4 0 00-5.7-5.7l-1 1" /><path d="M14 10a4 4 0 00-5.7 0l-3 3a4 4 0 005.7 5.7l1-1" /></Svg>;
export const Edit = (p: P) => <Svg {...p}><path d="M4 20h4L19 9l-4-4L4 16z" /><path d="M13.5 6.5l4 4" /></Svg>;
export const Beaker = (p: P) => <Svg {...p}><path d="M9 3.5h6M10 3.5v6L4.5 19a1.5 1.5 0 001.3 2.2h12.4a1.5 1.5 0 001.3-2.2L14 9.5v-6" /><path d="M7 15h10" /></Svg>;
export const Send = (p: P) => <Svg {...p}><path d="M4 12l16-8-6 16-2.5-6.5z" /><path d="M11.5 13.5L20 4" /></Svg>;
export const Save = (p: P) => <Svg {...p}><path d="M5 4h11l3 3v13H5z" /><path d="M8 4v5h7V4M8 20v-6h8v6" /></Svg>;
export const Undo = (p: P) => <Svg {...p}><path d="M9 14L4 9l5-5" /><path d="M4 9h10.5a5.5 5.5 0 010 11H11" /></Svg>;
export const Diamond = (p: P) => <Svg {...p}><path d="M12 3l9 9-9 9-9-9z" /></Svg>;
export const Eye = (p: P) => <Svg {...p}><path d="M2.5 12S6 5.5 12 5.5 21.5 12 21.5 12 18 18.5 12 18.5 2.5 12 2.5 12z" /><circle cx="12" cy="12" r="2.8" /></Svg>;
export const Scale = (p: P) => <Svg {...p}><path d="M12 4v16M7 20h10M5 8h14M5 8l-2.5 6a3 3 0 005 0zM19 8l-2.5 6a3 3 0 005 0z" /></Svg>;

export function Logo({ size = 26 }: { size?: number }) {
  return (
    <svg className="brand-mark" width={size} height={size} viewBox="0 0 32 32" aria-label="Kodehub">
      <defs>
        <linearGradient id="lg" x1="0" y1="0" x2="1" y2="1">
          <stop offset="0" stopColor="#8b9bff" />
          <stop offset="1" stopColor="#5eead4" />
        </linearGradient>
      </defs>
      <rect x="1" y="1" width="30" height="30" rx="8" fill="#121827" stroke="url(#lg)" strokeOpacity="0.6" />
      <g stroke="url(#lg)" strokeWidth="1.6" fill="none">
        <path d="M9 9L16 16L23 9M9 23L16 16L23 23M9 9V23M23 9V23" strokeOpacity="0.55" />
      </g>
      <g fill="url(#lg)">
        <circle cx="9" cy="9" r="2.4" /><circle cx="23" cy="9" r="2.4" /><circle cx="16" cy="16" r="3" />
        <circle cx="9" cy="23" r="2.4" /><circle cx="23" cy="23" r="2.4" />
      </g>
    </svg>
  );
}
