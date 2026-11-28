export function namePolicyError(error: unknown): string | undefined {
  const reason = String(error);
  if (reason.includes("name_is_role_word")) return "请填写家人的名字或昵称；妈妈、爸爸等称谓不能用作成员名字。";
  if (reason.includes("name_offensive")) return "请换一个礼貌的名字或昵称。这个名字没有保存。";
  if (reason.includes("name_invalid")) return "请填写一个有效的名字或昵称。";
  return undefined;
}
