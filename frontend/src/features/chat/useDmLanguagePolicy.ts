import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { dmLanguage } from "@/lib/api/language";
import { useAuth } from "@/store/auth";
import { useChat } from "@/store/chat";

function currentSession(owner: string, generation: number, account: string) {
  const auth = useAuth.getState();
  const active = useChat.getState().active;
  return (
    auth.user?.id === owner &&
    auth.generation === generation &&
    active?.kind === "account" &&
    active.id === account
  );
}

/** One policy read when opening an account DM; each send validates the latest policy. */
export function useDmLanguagePolicy(
  owner: string | null,
  generation: number,
  account: string | null,
) {
  const { t } = useTranslation();
  const identity = JSON.stringify([owner, generation, account]);
  const policyScope = useRef<{
    identity: string;
    active: boolean;
    validated: boolean;
  } | null>(null);
  const [notice, setNotice] = useState<{
    identity: string;
    language: string | null;
  } | null>(null);
  useEffect(() => {
    if (!owner || !account) return;
    const scope = { identity, active: true, validated: false };
    policyScope.current = scope;
    void dmLanguage.get(owner, account).then(
      (policy) => {
        if (
          scope.active &&
          !scope.validated &&
          currentSession(owner, generation, account)
        )
          setNotice({ identity, language: policy.language });
      },
      () => {
        /* Validation remains authoritative if the optional notice cannot load. */
      },
    );
    return () => {
      scope.active = false;
    };
  }, [owner, generation, account, identity]);

  const validateText = useCallback(
    async (text: string): Promise<string | null> => {
      const scope = policyScope.current;
      if (
        !scope?.active ||
        scope.identity !== identity ||
        !owner ||
        !account ||
        !currentSession(owner, generation, account)
      )
        return t("dmLanguage.validationFailed");
      try {
        const verdict = await dmLanguage.validate(owner, account, text);
        if (
          !scope.active ||
          policyScope.current !== scope ||
          !currentSession(owner, generation, account)
        )
          return t("dmLanguage.validationFailed");
        scope.validated = true;
        setNotice({ identity, language: verdict.language || null });
        if (verdict.allowed) return null;
        if (verdict.reason === "resource-limit")
          return t("dmLanguage.resourceLimit");
        if (verdict.reason === "unsupported-language")
          return t("dmLanguage.validationFailed");
        return t("dmLanguage.rejected", {
          language: t(`dmLanguage.languages.${verdict.language}`),
          percent: 80,
        });
      } catch {
        return t("dmLanguage.validationFailed");
      }
    },
    [owner, generation, account, identity, t],
  );
  return {
    language: notice?.identity === identity ? notice.language : null,
    validateText: account ? validateText : undefined,
  };
}
