import { useState } from "react";
import { motion } from "framer-motion";
import { Check, Plus, Loader2, Users } from "lucide-react";
import { useTranslation } from "react-i18next";
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogDescription,
  DialogTrigger,
} from "@/components/ui/dialog";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { IdentityCrest } from "@/components/identity";
import { cn } from "@/lib/utils";
import { fadeSlideUp, listStagger, useMotionOK } from "@/lib/motion";
import { useChat } from "@/store/chat";

export function CreateChannelDialog() {
  const { t } = useTranslation();
  const motionOK = useMotionOK();
  const peers = useChat((s) => s.peers);
  const createChannel = useChat((s) => s.createChannel);

  const [open, setOpen] = useState(false);
  const [name, setName] = useState("");
  const [selected, setSelected] = useState<Record<string, boolean>>({});
  const [busy, setBusy] = useState(false);

  const toggle = (id: string) => setSelected((s) => ({ ...s, [id]: !s[id] }));
  const selectedCount = Object.values(selected).filter(Boolean).length;

  const submit = async () => {
    const ids = Object.keys(selected).filter((k) => selected[k]);
    if (!name.trim()) return;
    setBusy(true);
    try {
      await createChannel(name.trim(), ids);
      setOpen(false);
      setName("");
      setSelected({});
    } finally {
      setBusy(false);
    }
  };

  return (
    <Dialog open={open} onOpenChange={setOpen}>
      <DialogTrigger asChild>
        <Button
          variant="ghost"
          size="icon"
          className="h-6 w-6"
          title={t("createChannel.trigger")}
        >
          <Plus className="h-4 w-4" />
        </Button>
      </DialogTrigger>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{t("createChannel.title")}</DialogTitle>
          <DialogDescription>
            {t("createChannel.description")}
          </DialogDescription>
        </DialogHeader>

        <Input
          autoFocus
          placeholder={t("createChannel.namePlaceholder")}
          value={name}
          onChange={(e) => setName(e.target.value)}
          aria-label={t("createChannel.namePlaceholder")}
        />

        <div className="space-y-1.5">
          <div className="flex items-center gap-2 font-mono text-[11px] font-semibold uppercase tracking-wider text-muted-foreground">
            <Users className="h-3.5 w-3.5" />
            {t("createChannel.invite")}
            {selectedCount > 0 && (
              <span className="text-signal">· {selectedCount}</span>
            )}
          </div>
          <motion.div
            initial={motionOK ? "hidden" : false}
            animate="visible"
            variants={listStagger}
            className="max-h-56 space-y-0.5 overflow-y-auto rounded-lg border p-1"
          >
            {peers.length === 0 && (
              <p className="px-2 py-3 text-center text-sm text-muted-foreground">
                {t("createChannel.noPeers")}
              </p>
            )}
            {peers.map((p) => {
              const on = !!selected[p.user_id];
              return (
                <motion.button
                  key={p.user_id}
                  variants={fadeSlideUp}
                  onClick={() => toggle(p.user_id)}
                  aria-pressed={on}
                  className={cn(
                    "flex w-full items-center gap-3 rounded-md px-2 py-1.5 text-left transition-colors",
                    on ? "bg-signal/10" : "hover:bg-accent/50",
                  )}
                >
                  <div className="min-w-0 flex-1">
                    <IdentityCrest
                      id={p.user_id}
                      name={p.name || t("common.unnamed")}
                      variant="compact"
                    />
                  </div>
                  <span
                    className={cn(
                      "flex h-5 w-5 shrink-0 items-center justify-center rounded-md border transition-colors",
                      on
                        ? "border-signal bg-signal text-primary-foreground"
                        : "border-input",
                    )}
                  >
                    {on && <Check className="h-3.5 w-3.5" />}
                  </span>
                </motion.button>
              );
            })}
          </motion.div>
        </div>

        <div className="flex justify-end gap-2">
          <Button variant="ghost" onClick={() => setOpen(false)}>
            {t("common.cancel")}
          </Button>
          <Button disabled={!name.trim() || busy} onClick={submit}>
            {busy && <Loader2 className="h-4 w-4 animate-spin" />}
            {t("common.create")}
          </Button>
        </div>
      </DialogContent>
    </Dialog>
  );
}
