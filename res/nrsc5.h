#ifndef NRSC5_H
#define NRSC5_H

#ifdef __cplusplus
extern "C" {
#endif

typedef struct nrsc5_t nrsc5_t;

int nrsc5_open(nrsc5_t **st, int device_index);
void nrsc5_close(nrsc5_t *st);

#ifdef __cplusplus
}
#endif

#endif
