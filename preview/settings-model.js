/*
 * Pure state machine for the touch UI. Does not own SVG rendering or storage.
 * The same conceptual navigation is used in firmware/face_settings.c.
 */
export class AppearanceSettings {
  constructor(themeIds, activeId) {
    if (!Array.isArray(themeIds) || !themeIds.includes(activeId))
      throw new Error("Unknown initial appearance");
    this.ids = new Set(themeIds);
    this.activeId = activeId;
    this.selectedId = activeId;
    this.page = "face";
    this.gearVisible = false;
    this.gearDeadline = 0;
  }
  tapFace(now) {
    if(this.page !== "face")return;
    this.gearVisible = true;
    this.gearDeadline = now + 5000;
  }
  tick(now) {
    if(this.page==="face" && this.gearVisible && now >= this.gearDeadline)
      this.gearVisible=false;
  }
  openSettings() {
    if(this.page!=="face" || !this.gearVisible)return false;
    this.gearVisible=false;
    this.page="settings";
    return true;
  }
  openAppearance() {
    if(this.page!=="settings")return false;
    this.selectedId=this.activeId;
    this.page="appearance";
    return true;
  }
  select(id) {
    if(this.page!=="appearance" || !this.ids.has(id))return false;
    this.selectedId=id; /* not yet applied */
    return true;
  }
  back() {
    if(this.page==="appearance") {
      this.page="settings";
      this.selectedId=this.activeId;
      return true;
    }
    if(this.page==="settings")return this.close();
    return false;
  }
  close() {
    if(this.page==="face")return false;
    this.page="face";
    this.gearVisible=false;
    this.selectedId=this.activeId;
    return true;
  }
  apply() {
    if(this.page!=="appearance")return null;
    const chosen=this.selectedId;
    this.activeId=chosen;
    this.close(); /* returns directly to face */
    return chosen;
  }
}
