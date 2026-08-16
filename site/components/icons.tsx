import {
  Rocket,
  Lightbulb,
  BookOpen,
  Server,
  FileCode,
  Terminal,
  Cpu,
  Settings,
  FlaskConical,
  ShieldHalf,
  Clock,
  type LucideIcon,
} from "lucide-react";

export const iconMap: Record<string, LucideIcon> = {
  rocket: Rocket,
  lightbulb: Lightbulb,
  "book-open": BookOpen,
  server: Server,
  "file-code": FileCode,
  terminal: Terminal,
  api: Cpu,
  gear: Settings,
  "flask-conical": FlaskConical,
  "shield-half": ShieldHalf,
  clock: Clock,
};

export function GroupIcon({ name, className }: { name?: string; className?: string }) {
  if (!name) return null;
  const Icon = iconMap[name] ?? Rocket;
  return <Icon className={className} />;
}
